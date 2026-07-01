//! Retrieval — the unified query layer (§949–§952).
//!
//! [`plan_retrieval`] turns a question into a [`RetrievalStrategy`] (§949), then
//! [`Retriever::search`] runs hybrid search (§950): **BM25** keyword ranking over
//! the document store, vector-similarity search, and label search over the
//! knowledge graph with multi-hop expansion (§951–§952). The three ranked lists
//! are then combined with Reciprocal Rank Fusion (`1/(k+rank)` summed across
//! sources) — rank-based, so the different score scales of BM25, cosine and
//! graph matching don't distort the result and corroborated items rise.
//!
//! Scores fold in each item's confidence (§948), so low-confidence knowledge
//! ranks below high-confidence knowledge for the same textual match.
//!
//! With the default [`HashingEmbedder`](ckos_memory::HashingEmbedder), "vector"
//! is a second lexical-overlap signal, not a semantic one — it cannot relate a
//! paraphrase or synonym sharing no words with the query (see
//! `ckos_memory::embedding`'s module doc). Swap in a real embedding model
//! behind the same [`Embedder`] trait for genuine semantic recall.

use ckos_graph::KnowledgeGraph;
use ckos_memory::{cosine, Embedder, Query, Storage};
use std::collections::VecDeque;

/// Which source a hit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HitSource {
    /// Keyword match in the document store.
    Keyword,
    /// Vector (embedding) similarity match in the document store (§944, §950).
    Vector,
    /// Direct label match in the knowledge graph.
    Graph,
    /// Reached by graph traversal from a direct match (§952).
    GraphHop,
}

/// A single ranked retrieval result.
#[derive(Debug, Clone)]
pub struct Hit {
    /// Display title (document title or node label).
    pub title: String,
    /// Short context snippet.
    pub snippet: String,
    /// Relevance score (higher is better).
    pub score: f32,
    /// Where the hit originated.
    pub source: HitSource,
}

/// The plan chosen for a question (§949).
#[derive(Debug, Clone, Copy)]
pub struct RetrievalStrategy {
    /// Run keyword search over documents.
    pub keyword: bool,
    /// Run vector-similarity search over documents (needs an embedder).
    pub vector: bool,
    /// Run label search over the graph.
    pub graph: bool,
    /// How many hops to expand graph matches (§952).
    pub max_hops: usize,
}

/// Decide a retrieval strategy from the question text (§949).
///
/// Relational phrasing ("related to", "depends on", "who maintains") implies
/// the graph and deeper traversal; otherwise keyword search leads with a single
/// hop of graph context.
pub fn plan_retrieval(question: &str) -> RetrievalStrategy {
    let q = question.to_lowercase();
    let relational = [
        "related",
        "depend",
        "maintain",
        "connected",
        "between",
        "reference",
        "implement",
    ]
    .iter()
    .any(|k| q.contains(k));
    RetrievalStrategy {
        keyword: true,
        vector: true,
        graph: true,
        max_hops: if relational { 2 } else { 1 },
    }
}

/// Split text into lowercase tokens longer than one character.
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(String::from)
        .collect()
}

/// Alias used at call sites that read as "query terms".
fn terms(query: &str) -> Vec<String> {
    tokens(query)
}

/// Very common words excluded from query-expansion candidates.
const EXPANSION_STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "was", "were", "with", "that", "this", "from", "have", "has",
    "had", "but", "not", "you", "all", "any", "its", "their", "they", "she", "him", "her", "his",
    "our", "your", "via", "per", "into", "onto", "over", "under", "than", "then", "them", "out",
];

/// Expand `query` with terms drawn from pseudo-relevant `feedback` texts
/// (pseudo-relevance feedback à la Rocchio/RM3). The most frequent informative
/// terms in the feedback set that aren't already in the query are appended, so a
/// second retrieval pass recalls documents the original wording missed. Returns
/// the original query unchanged when no useful term is found.
pub fn expand_query(query: &str, feedback: &[String], max_terms: usize) -> String {
    use std::collections::HashMap;
    if max_terms == 0 {
        return query.to_string();
    }
    let existing: std::collections::HashSet<String> = tokens(query).into_iter().collect();
    let mut freq: HashMap<String, usize> = HashMap::new();
    for text in feedback {
        for tok in tokens(text) {
            if tok.chars().count() > 2
                && !existing.contains(&tok)
                && !EXPANSION_STOPWORDS.contains(&tok.as_str())
            {
                *freq.entry(tok).or_default() += 1;
            }
        }
    }
    let mut ranked: Vec<(String, usize)> = freq.into_iter().collect();
    // Most frequent first; alphabetical tie-break for deterministic output.
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let expansion: Vec<String> = ranked.into_iter().take(max_terms).map(|(t, _)| t).collect();
    if expansion.is_empty() {
        query.to_string()
    } else {
        format!("{query} {}", expansion.join(" "))
    }
}

fn count_matches(haystack: &str, term: &str) -> usize {
    haystack.matches(term).count()
}

/// BM25 saturation parameters (standard defaults).
const BM25_K1: f32 = 1.5;
const BM25_B: f32 = 0.75;
/// Title terms count for more than body terms (field boost).
const TITLE_BOOST: f32 = 2.0;

/// Count occurrences of `term` among already-tokenized text.
fn tf(tokens: &[String], term: &str) -> usize {
    tokens.iter().filter(|t| t.as_str() == term).count()
}

/// Weight applied to cosine similarity so vector hits are comparable in scale
/// to keyword/graph hits.
const VECTOR_WEIGHT: f32 = 5.0;
/// Minimum cosine similarity for a vector hit to be considered relevant.
const VECTOR_THRESHOLD: f32 = 0.2;
/// Reciprocal Rank Fusion constant; the standard default damps the weight of
/// top ranks so lower ranks still contribute (Cormack et al.; used by
/// Elasticsearch/LangChain).
const RRF_K: f32 = 60.0;
/// Per-hop score decay for graph traversal hits: a hop-N neighbour scores
/// `HOP_DECAY^N` of the direct match, so a 2-hop hit is weaker than a 1-hop hit
/// rather than receiving the same flat penalty.
const HOP_DECAY: f32 = 0.4;

/// Sort a source's hits by descending score (its rank order for fusion).
fn ranked(mut hits: Vec<Hit>) -> Vec<Hit> {
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
    });
    hits
}

/// Lexical similarity (Jaccard over title+snippet tokens) between two hits —
/// the redundancy signal for MMR, needing no stored embeddings.
fn hit_similarity(a: &Hit, b: &Hit) -> f32 {
    let ta: std::collections::HashSet<String> = tokens(&format!("{} {}", a.title, a.snippet))
        .into_iter()
        .collect();
    let tb: std::collections::HashSet<String> = tokens(&format!("{} {}", b.title, b.snippet))
        .into_iter()
        .collect();
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    inter / union
}

/// Re-rank hits with **Maximal Marginal Relevance** (Carbonell & Goldstein,
/// SIGIR 1998) to reduce redundancy in the result set. At each step it picks the
/// hit maximizing `λ·relevance − (1−λ)·max similarity-to-already-selected`.
/// `lambda` in `[0,1]`: 1 = pure relevance (original order), 0 = pure diversity.
/// Relevance is each hit's score normalized by the max; redundancy is lexical
/// [`hit_similarity`]. Returns up to `k` hits.
pub fn mmr_rerank(hits: &[Hit], lambda: f32, k: usize) -> Vec<Hit> {
    let lambda = lambda.clamp(0.0, 1.0);
    let max_score = hits
        .iter()
        .map(|h| h.score)
        .fold(0.0_f32, f32::max)
        .max(1e-9);
    let mut remaining: Vec<&Hit> = hits.iter().collect();
    let mut selected: Vec<Hit> = Vec::new();
    while !remaining.is_empty() && selected.len() < k {
        let mut best_idx = 0;
        let mut best_val = f32::NEG_INFINITY;
        for (i, cand) in remaining.iter().enumerate() {
            let relevance = cand.score / max_score;
            let redundancy = selected
                .iter()
                .map(|s| hit_similarity(cand, s))
                .fold(0.0_f32, f32::max);
            let mmr = lambda * relevance - (1.0 - lambda) * redundancy;
            if mmr > best_val {
                best_val = mmr;
                best_idx = i;
            }
        }
        selected.push(remaining.remove(best_idx).clone());
    }
    selected
}

/// Combine per-source ranked lists with **Reciprocal Rank Fusion**: each item's
/// fused score is `sum over sources of 1/(RRF_K + rank)`. Because it uses ranks,
/// not raw scores, the wildly different score scales of BM25, cosine and graph
/// matching don't distort the result, and items corroborated by several sources
/// naturally rise. Items are collapsed by title; the highest-scoring occurrence
/// supplies the displayed snippet/source. Returns up to `limit` hits.
fn fuse_rrf(lists: Vec<Vec<Hit>>, limit: usize) -> Vec<Hit> {
    use std::collections::HashMap;
    let mut acc: HashMap<String, (f32, Hit)> = HashMap::new();
    for list in lists {
        for (rank, hit) in list.into_iter().enumerate() {
            let contrib = 1.0 / (RRF_K + (rank + 1) as f32);
            let entry = acc.entry(hit.title.clone()).or_insert((0.0, hit.clone()));
            entry.0 += contrib;
            if hit.score > entry.1.score {
                entry.1 = hit; // best representative for display
            }
        }
    }
    let mut fused: Vec<Hit> = acc
        .into_values()
        .map(|(rrf, mut hit)| {
            hit.score = rrf; // fused relevance score
            hit
        })
        .collect();
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
    });
    fused.truncate(limit);
    fused
}

/// Runs hybrid search across a store and a graph (§950).
pub struct Retriever<'a> {
    store: &'a dyn Storage,
    graph: &'a KnowledgeGraph,
    embedder: Option<&'a dyn Embedder>,
}

impl<'a> Retriever<'a> {
    /// Build a retriever over the given knowledge sources (no vector search).
    pub fn new(store: &'a dyn Storage, graph: &'a KnowledgeGraph) -> Self {
        Retriever {
            store,
            graph,
            embedder: None,
        }
    }

    /// Build a retriever that also runs vector-similarity search (§944, §950).
    pub fn with_embedder(
        store: &'a dyn Storage,
        graph: &'a KnowledgeGraph,
        embedder: &'a dyn Embedder,
    ) -> Self {
        Retriever {
            store,
            graph,
            embedder: Some(embedder),
        }
    }

    /// Plan and execute retrieval, returning up to `limit` ranked hits. The
    /// per-source result lists are combined with Reciprocal Rank Fusion (§950).
    pub fn search(&self, question: &str, limit: usize) -> Vec<Hit> {
        let strategy = plan_retrieval(question);
        let terms = terms(question);
        let mut lists: Vec<Vec<Hit>> = Vec::new();

        if strategy.keyword {
            lists.push(ranked(self.keyword_hits(&terms)));
        }
        if strategy.vector {
            if let Some(embedder) = self.embedder {
                lists.push(ranked(self.vector_hits(&embedder.embed(question))));
            }
        }
        if strategy.graph {
            lists.push(ranked(self.graph_hits(&terms, strategy.max_hops)));
        }

        fuse_rrf(lists, limit)
    }

    /// Two-pass search with pseudo-relevance feedback (§949). The first pass'
    /// top documents seed [`expand_query`], and the expanded query drives the
    /// returned search — recalling documents the original phrasing missed.
    /// `feedback_docs` controls how many top docs feed expansion; `max_terms`
    /// how many terms are added.
    pub fn search_expanded(
        &self,
        question: &str,
        limit: usize,
        feedback_docs: usize,
        max_terms: usize,
    ) -> Vec<Hit> {
        let docs = self
            .store
            .search(&Query {
                text: Some(question.to_string()),
                limit: feedback_docs,
                ..Default::default()
            })
            .unwrap_or_default();
        let feedback: Vec<String> = docs
            .iter()
            .map(|d| format!("{} {}", d.title, d.body))
            .collect();
        let expanded = expand_query(question, &feedback, max_terms);
        self.search(&expanded, limit)
    }

    /// Search after expanding the query with a [`SynonymTable`](crate::synonyms::SynonymTable)
    /// (§949). Unlike [`search_expanded`](Self::search_expanded) (pseudo-relevance
    /// feedback, which only recalls documents reachable from terms *already*
    /// found by literal overlap), this injects a priori related terms — so it
    /// can recall a document sharing zero words with the query, closing the
    /// vocabulary-mismatch gap the default hashing embedder cannot (see
    /// `ckos_memory::embedding`'s module doc).
    pub fn search_synonyms(
        &self,
        question: &str,
        limit: usize,
        table: &crate::synonyms::SynonymTable,
    ) -> Vec<Hit> {
        let expanded = crate::synonyms::expand_query_with_synonyms(question, table, 10);
        self.search(&expanded, limit)
    }

    /// Search, then diversify with MMR (§949–§950). Over-fetches a candidate pool
    /// and re-ranks it down to `limit` so near-duplicate results don't crowd out
    /// distinct, still-relevant ones. `lambda` trades relevance (1.0) against
    /// diversity (0.0); ~0.7 is a sensible default.
    pub fn search_diverse(&self, question: &str, limit: usize, lambda: f32) -> Vec<Hit> {
        let pool = self.search(question, (limit * 4).max(limit));
        mmr_rerank(&pool, lambda, limit)
    }

    /// Keyword search over the document store using **BM25** ranking — terms that
    /// are rare across the corpus weigh more (IDF), term frequency saturates, and
    /// long documents are length-normalized. Title hits get a field boost and the
    /// score scales by document confidence (§948). BM25 is the standard lexical
    /// half of hybrid search (§950).
    fn keyword_hits(&self, terms: &[String]) -> Vec<Hit> {
        let docs = self.store.search(&Query::default()).unwrap_or_default();
        if docs.is_empty() || terms.is_empty() {
            return Vec::new();
        }

        // Tokenize once; track per-doc title/body tokens and effective length.
        let tokenized: Vec<(Vec<String>, Vec<String>)> = docs
            .iter()
            .map(|d| (tokens(&d.title), tokens(&d.body)))
            .collect();
        let n = docs.len() as f32;
        let avgdl = {
            let total: usize = tokenized.iter().map(|(t, b)| t.len() + b.len()).sum();
            (total as f32 / n).max(1.0)
        };

        // Document frequency per query term (docs containing it in title or body).
        let idf = |term: &str| -> f32 {
            let df = tokenized
                .iter()
                .filter(|(t, b)| tf(t, term) + tf(b, term) > 0)
                .count() as f32;
            // BM25+ idf form: always >= 0.
            ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
        };
        let idfs: Vec<f32> = terms.iter().map(|t| idf(t)).collect();

        let mut hits = Vec::new();
        for (doc, (title_tokens, body_tokens)) in docs.iter().zip(&tokenized) {
            let dl = (title_tokens.len() + body_tokens.len()) as f32;
            let mut score = 0.0f32;
            for (term, &idf) in terms.iter().zip(&idfs) {
                let tf_eff =
                    TITLE_BOOST * tf(title_tokens, term) as f32 + tf(body_tokens, term) as f32;
                if tf_eff == 0.0 {
                    continue;
                }
                let denom = tf_eff + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / avgdl);
                score += idf * (tf_eff * (BM25_K1 + 1.0)) / denom;
            }
            if score > 0.0 {
                score *= doc.confidence as f32 / 100.0;
                hits.push(Hit {
                    title: doc.title.clone(),
                    snippet: doc.body.chars().take(80).collect(),
                    score,
                    source: HitSource::Keyword,
                });
            }
        }
        hits
    }

    /// Vector-similarity search over documents that carry embeddings (§950).
    fn vector_hits(&self, query_embedding: &[f32]) -> Vec<Hit> {
        let docs = self.store.search(&Query::default()).unwrap_or_default();
        let mut hits = Vec::new();
        for doc in docs {
            let Some(embedding) = &doc.embedding else {
                continue;
            };
            let sim = cosine(query_embedding, embedding);
            if sim >= VECTOR_THRESHOLD {
                hits.push(Hit {
                    title: doc.title.clone(),
                    snippet: doc.body.chars().take(80).collect(),
                    score: sim * VECTOR_WEIGHT * (doc.confidence as f32 / 100.0),
                    source: HitSource::Vector,
                });
            }
        }
        hits
    }

    /// Label search over the graph plus multi-hop expansion (§951–§952). Direct
    /// matches are boosted by PageRank centrality so influential nodes outrank
    /// peripheral ones (the Graph-RAG node-importance signal, §951).
    fn graph_hits(&self, terms: &[String], max_hops: usize) -> Vec<Hit> {
        let pr = self.graph.pagerank(0.85, 20);
        let max_pr = pr
            .values()
            .cloned()
            .fold(0.0_f32, f32::max)
            .max(f32::EPSILON);
        let mut hits = Vec::new();
        for node in self.graph.nodes() {
            let label = node.label.to_lowercase();
            let matches: usize = terms.iter().map(|t| count_matches(&label, t)).sum();
            if matches == 0 {
                continue;
            }
            // Centrality in 0..1; boost direct matches up to 2x for the hub.
            let centrality = pr.get(&node.id).copied().unwrap_or(0.0) / max_pr;
            let base = matches as f32 * (node.confidence as f32 / 100.0) * 3.0 * (1.0 + centrality);
            hits.push(Hit {
                title: node.label.clone(),
                snippet: format!("{:?}", node.kind),
                score: base,
                source: HitSource::Graph,
            });
            // Expand: neighbours reachable within max_hops, score geometrically
            // decayed by actual hop distance (a 2-hop neighbour scores lower than
            // a 1-hop one, not the same).
            if max_hops > 1 {
                for (neighbor, hops) in self.graph.traverse_with_hops(&node.id, max_hops) {
                    hits.push(Hit {
                        title: neighbor.label.clone(),
                        snippet: format!("{:?} (via {})", neighbor.kind, node.label),
                        score: base * HOP_DECAY.powi(hops as i32),
                        source: HitSource::GraphHop,
                    });
                }
            }
        }
        hits
    }
}

/// An LRU cache of query → ranked hits (§958 search cache). Repeated popular
/// queries skip the hybrid-search work. Bounded capacity; the least-recently
/// used entry is evicted when full.
pub struct SearchCache {
    capacity: usize,
    entries: std::collections::HashMap<String, Vec<Hit>>,
    /// Front = least-recently used, back = most-recently used.
    order: VecDeque<String>,
}

impl SearchCache {
    /// Create a cache holding at most `capacity` queries (min 1).
    pub fn new(capacity: usize) -> Self {
        SearchCache {
            capacity: capacity.max(1),
            entries: std::collections::HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn touch(&mut self, query: &str) {
        if let Some(pos) = self.order.iter().position(|q| q == query) {
            self.order.remove(pos);
        }
        self.order.push_back(query.to_string());
    }

    /// Look up cached hits for a query, marking it most-recently used.
    pub fn get(&mut self, query: &str) -> Option<Vec<Hit>> {
        if self.entries.contains_key(query) {
            self.touch(query);
            self.entries.get(query).cloned()
        } else {
            None
        }
    }

    /// Cache the hits for a query, evicting the LRU entry if over capacity.
    pub fn put(&mut self, query: impl Into<String>, hits: Vec<Hit>) {
        let query = query.into();
        self.entries.insert(query.clone(), hits);
        self.touch(&query);
        while self.entries.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            }
        }
    }

    /// Number of cached queries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckos_graph::{EdgeKind, NodeKind};
    use ckos_memory::{Document, InMemoryStore};

    #[test]
    fn planner_deepens_for_relational_questions() {
        assert_eq!(plan_retrieval("what is a transformer").max_hops, 1);
        assert_eq!(plan_retrieval("what depends on the kernel").max_hops, 2);
    }

    #[test]
    fn search_cache_hits_misses_and_evicts_lru() {
        let hit = |title: &str| Hit {
            title: title.into(),
            snippet: String::new(),
            score: 1.0,
            source: HitSource::Keyword,
        };
        let mut cache = SearchCache::new(2);
        assert!(cache.get("a").is_none()); // miss
        cache.put("a", vec![hit("ra")]);
        cache.put("b", vec![hit("rb")]);
        // Hit refreshes "a" as most-recently used.
        assert_eq!(cache.get("a").unwrap()[0].title, "ra");
        // Adding "c" evicts the LRU, which is now "b".
        cache.put("c", vec![hit("rc")]);
        assert_eq!(cache.len(), 2);
        assert!(cache.get("b").is_none());
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn keyword_ranks_title_matches_above_body() {
        let mut store = InMemoryStore::new();
        store
            .write(Document::new(
                "note",
                "kernel design",
                "scheduling internals",
            ))
            .unwrap();
        store
            .write(Document::new(
                "note",
                "scheduling",
                "mentions the kernel once",
            ))
            .unwrap();
        let graph = KnowledgeGraph::new();
        let retriever = Retriever::new(&store, &graph);
        let hits = retriever.search("kernel", 10);
        assert_eq!(hits.len(), 2);
        // Title match ("kernel design") outranks the body-only mention.
        assert_eq!(hits[0].title, "kernel design");
    }

    #[test]
    fn bm25_weights_rare_terms_higher() {
        let mut store = InMemoryStore::new();
        // Five docs make "common" a frequent (low-IDF) term.
        for i in 0..5 {
            store
                .write(Document::new("note", format!("c{i}"), "common"))
                .unwrap();
        }
        store.write(Document::new("note", "A", "common")).unwrap(); // common only
        store.write(Document::new("note", "B", "rare")).unwrap(); // rare term
        let graph = KnowledgeGraph::new();
        let r = Retriever::new(&store, &graph);
        let hits = r.search("common rare", 10);
        let score = |t: &str| hits.iter().find(|h| h.title == t).map(|h| h.score).unwrap();
        // The rare-term match outranks the common-term match (higher IDF).
        assert!(
            score("B") > score("A"),
            "rare {} should beat common {}",
            score("B"),
            score("A")
        );
    }

    #[test]
    fn expand_query_adds_feedback_terms() {
        let feedback = vec![
            "the scheduler dispatches tasks".to_string(),
            "scheduler priority queue".to_string(),
        ];
        let expanded = expand_query("kernel", &feedback, 2);
        // Original term kept; "scheduler" (frequency 2) is added; "the" filtered.
        assert!(expanded.starts_with("kernel "));
        assert!(expanded.contains("scheduler"));
        assert!(!expanded.contains("the "));
        // No feedback → unchanged.
        assert_eq!(expand_query("kernel", &[], 3), "kernel");
    }

    #[test]
    fn search_expanded_recalls_what_the_original_missed() {
        let mut store = InMemoryStore::new();
        store
            .write(Document::new(
                "note",
                "kernel",
                "the scheduler dispatches tasks",
            ))
            .unwrap();
        store
            .write(Document::new(
                "note",
                "scheduler internals",
                "priority queue",
            ))
            .unwrap();
        let graph = KnowledgeGraph::new();
        let r = Retriever::new(&store, &graph);

        // Plain search for "kernel" misses the scheduler doc (no "kernel" in it).
        let plain = r.search("kernel", 10);
        assert!(!plain.iter().any(|h| h.title == "scheduler internals"));

        // Expanding from the top doc's body ("scheduler") recalls it.
        let expanded = r.search_expanded("kernel", 10, 3, 3);
        assert!(expanded.iter().any(|h| h.title == "scheduler internals"));
    }

    #[test]
    fn graph_hop_hits_decay_with_distance() {
        // a -> b -> c: b is 1 hop from a, c is 2 hops. c must score lower than b.
        let store = InMemoryStore::new();
        let mut graph = KnowledgeGraph::new();
        let a = graph.add_node(NodeKind::Concept, "root query term", 100);
        let b = graph.add_node(NodeKind::Concept, "near", 100);
        let c = graph.add_node(NodeKind::Concept, "far", 100);
        graph.connect(&a, &b, EdgeKind::RelatedTo);
        graph.connect(&b, &c, EdgeKind::RelatedTo);

        // "related to" phrasing selects max_hops=2 in plan_retrieval.
        let hits = Retriever::new(&store, &graph).search("what is related to root query term", 10);
        let near = hits.iter().find(|h| h.title == "near").unwrap();
        let far = hits.iter().find(|h| h.title == "far").unwrap();
        assert!(
            near.score > far.score,
            "1-hop ({}) must outscore 2-hop ({})",
            near.score,
            far.score
        );
    }

    #[test]
    fn graph_hits_favor_central_nodes() {
        // Two nodes match "node"; the central one (a hub others point to) should
        // outrank the peripheral one thanks to the PageRank boost.
        let store = InMemoryStore::new();
        let mut graph = KnowledgeGraph::new();
        let core = graph.add_node(NodeKind::Concept, "Core node", 100);
        let leaf = graph.add_node(NodeKind::Concept, "Leaf node", 100);
        let x = graph.add_node(NodeKind::Concept, "x", 100);
        let y = graph.add_node(NodeKind::Concept, "y", 100);
        graph.connect(&leaf, &core, EdgeKind::References);
        graph.connect(&x, &core, EdgeKind::References);
        graph.connect(&y, &core, EdgeKind::References);

        let hits = Retriever::new(&store, &graph).search("node", 10);
        let core_pos = hits.iter().position(|h| h.title == "Core node").unwrap();
        let leaf_pos = hits.iter().position(|h| h.title == "Leaf node").unwrap();
        assert!(core_pos < leaf_pos, "central node should rank first");
    }

    #[test]
    fn mmr_trades_relevance_for_diversity() {
        let hit = |title: &str, snippet: &str, score: f32| Hit {
            title: title.into(),
            snippet: snippet.into(),
            score,
            source: HitSource::Keyword,
        };
        // h2 is a near-duplicate of h1; h3 is distinct but slightly less relevant.
        let hits = vec![
            hit("Transformer", "attention mechanism", 1.0),
            hit("Transformer model", "attention mechanism", 0.9),
            hit("Scheduler", "task queue priority", 0.8),
        ];

        // Pure relevance (λ=1): the near-duplicate stays second.
        let relevance_only = mmr_rerank(&hits, 1.0, 3);
        assert_eq!(relevance_only[1].title, "Transformer model");

        // Balanced (λ=0.5): the distinct result is promoted over the duplicate.
        let diversified = mmr_rerank(&hits, 0.5, 3);
        assert_eq!(diversified[0].title, "Transformer");
        assert_eq!(diversified[1].title, "Scheduler");
    }

    #[test]
    fn rrf_is_rank_based_not_score_scale() {
        // A doc ranked #1 by the (small-magnitude) vector source and #1 by the
        // keyword source must fuse to the top — RRF ignores raw score scale.
        use ckos_memory::{Embedder, HashingEmbedder};
        let embedder = HashingEmbedder::new(64);
        let mut store = InMemoryStore::new();
        let mut both = Document::new("note", "Kernel", "kernel scheduling");
        both.embedding = Some(embedder.embed("kernel scheduling"));
        store.write(both).unwrap();
        let mut other = Document::new("note", "Other", "kernel"); // keyword-only, weaker
        other.embedding = Some(embedder.embed("unrelated text"));
        store.write(other).unwrap();

        let graph = KnowledgeGraph::new();
        let hits = Retriever::with_embedder(&store, &graph, &embedder).search("kernel", 10);
        // The doubly-corroborated doc tops the list despite cosine being ~0.x
        // while BM25 is a different magnitude.
        assert_eq!(hits[0].title, "Kernel");
    }

    #[test]
    fn corroboration_across_sources_boosts_score() {
        let mut store = InMemoryStore::new();
        store
            .write(Document::new("note", "Graphlib", "Graphlib is a tool"))
            .unwrap();
        let mut graph = KnowledgeGraph::new();
        graph.add_node(NodeKind::Tool, "Graphlib", 100);
        let empty = KnowledgeGraph::new();

        let with_graph = Retriever::new(&store, &graph).search("Graphlib", 10);
        let keyword_only = Retriever::new(&store, &empty).search("Graphlib", 10);
        let c = with_graph
            .iter()
            .find(|h| h.title == "Graphlib")
            .unwrap()
            .score;
        let s = keyword_only
            .iter()
            .find(|h| h.title == "Graphlib")
            .unwrap()
            .score;
        // Two corroborating sources (keyword + graph) outrank one.
        assert!(c > s, "corroborated {c} should exceed single-source {s}");
    }

    #[test]
    fn vector_source_finds_docs_keyword_misses() {
        use ckos_memory::{Embedder, HashingEmbedder};
        let embedder = HashingEmbedder::new(128);
        let mut store = InMemoryStore::new();
        // Title and body carry none of the query terms, but the embedding was
        // computed from richer content — so only vector search can surface it
        // (the realistic case: embeddings derived from full text, snippet differs).
        let mut doc = Document::new("note", "doc-a", "placeholder snippet");
        doc.embedding = Some(embedder.embed("kernel priority scheduler queue dispatch"));
        store.write(doc).unwrap();

        let graph = KnowledgeGraph::new();
        let retriever = Retriever::with_embedder(&store, &graph, &embedder);
        let hits = retriever.search("kernel priority", 10);
        // No keyword/graph match exists, so the surviving hit is vector-sourced.
        assert!(hits.iter().any(|h| h.source == HitSource::Vector));

        // Without an embedder the same query finds nothing.
        let plain = Retriever::new(&store, &graph);
        assert!(plain.search("kernel priority", 10).is_empty());
    }

    #[test]
    fn hybrid_combines_graph_and_keyword_with_hops() {
        let mut store = InMemoryStore::new();
        store
            .write(Document::new("note", "CKOS overview", "the CKOS project"))
            .unwrap();
        let mut graph = KnowledgeGraph::new();
        let ckos = graph.add_node(NodeKind::Project, "CKOS", 100);
        let sched = graph.add_node(NodeKind::Tool, "scheduler", 90);
        graph.connect(&ckos, &sched, EdgeKind::DependsOn);

        let retriever = Retriever::new(&store, &graph);
        // Relational question → 2 hops, so the scheduler is reached via CKOS.
        let hits = retriever.search("what does CKOS depend on", 10);
        assert!(hits.iter().any(|h| h.source == HitSource::Keyword));
        assert!(hits.iter().any(|h| h.title == "CKOS"));
        assert!(hits
            .iter()
            .any(|h| h.title == "scheduler" && h.source == HitSource::GraphHop));
    }
}
