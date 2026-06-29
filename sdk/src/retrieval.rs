//! Retrieval — the unified query layer (§949–§952).
//!
//! [`plan_retrieval`] turns a question into a [`RetrievalStrategy`] (§949), then
//! [`Retriever::search`] runs hybrid search (§950): keyword search over the
//! document store and label search over the knowledge graph, with multi-hop
//! expansion of graph matches (§951–§952). Results from both sources are scored,
//! deduplicated and ranked into one list.
//!
//! Scores fold in each item's confidence (§948), so low-confidence knowledge
//! ranks below high-confidence knowledge for the same textual match.

use ckos_graph::KnowledgeGraph;
use ckos_memory::{Query, Storage};

/// Which source a hit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitSource {
    /// Keyword match in the document store.
    Keyword,
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
        graph: true,
        max_hops: if relational { 2 } else { 1 },
    }
}

/// Split a query into lowercase terms longer than one character.
fn terms(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(String::from)
        .collect()
}

fn count_matches(haystack: &str, term: &str) -> usize {
    haystack.matches(term).count()
}

/// Runs hybrid search across a store and a graph (§950).
pub struct Retriever<'a> {
    store: &'a dyn Storage,
    graph: &'a KnowledgeGraph,
}

impl<'a> Retriever<'a> {
    /// Build a retriever over the given knowledge sources.
    pub fn new(store: &'a dyn Storage, graph: &'a KnowledgeGraph) -> Self {
        Retriever { store, graph }
    }

    /// Plan and execute retrieval, returning up to `limit` ranked hits.
    pub fn search(&self, question: &str, limit: usize) -> Vec<Hit> {
        let strategy = plan_retrieval(question);
        let terms = terms(question);
        let mut hits: Vec<Hit> = Vec::new();

        if strategy.keyword {
            hits.extend(self.keyword_hits(&terms));
        }
        if strategy.graph {
            hits.extend(self.graph_hits(&terms, strategy.max_hops));
        }

        // Deduplicate by title, keeping the highest score.
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        let mut seen = std::collections::HashSet::new();
        hits.retain(|h| seen.insert(h.title.clone()));
        hits.truncate(limit);
        hits
    }

    /// Keyword search over the document store, weighting title over body and
    /// scaling by document confidence (§948).
    fn keyword_hits(&self, terms: &[String]) -> Vec<Hit> {
        let docs = self.store.search(&Query::default()).unwrap_or_default();
        let mut hits = Vec::new();
        for doc in docs {
            let title = doc.title.to_lowercase();
            let body = doc.body.to_lowercase();
            let mut score = 0.0f32;
            for t in terms {
                score += 2.0 * count_matches(&title, t) as f32;
                score += count_matches(&body, t) as f32;
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

    /// Label search over the graph plus multi-hop expansion (§951–§952).
    fn graph_hits(&self, terms: &[String], max_hops: usize) -> Vec<Hit> {
        let mut hits = Vec::new();
        for node in self.graph.nodes() {
            let label = node.label.to_lowercase();
            let matches: usize = terms.iter().map(|t| count_matches(&label, t)).sum();
            if matches == 0 {
                continue;
            }
            let base = matches as f32 * (node.confidence as f32 / 100.0) * 3.0;
            hits.push(Hit {
                title: node.label.clone(),
                snippet: format!("{:?}", node.kind),
                score: base,
                source: HitSource::Graph,
            });
            // Expand: neighbours reachable within max_hops, score decayed by hop.
            if max_hops > 1 {
                for neighbor in self.graph.traverse(&node.id, max_hops) {
                    hits.push(Hit {
                        title: neighbor.label.clone(),
                        snippet: format!("{:?} (via {})", neighbor.kind, node.label),
                        score: base * 0.4,
                        source: HitSource::GraphHop,
                    });
                }
            }
        }
        hits
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
    fn keyword_ranks_title_matches_above_body() {
        let mut store = InMemoryStore::new();
        store
            .write(Document::new("note", "kernel design", "scheduling internals"))
            .unwrap();
        store
            .write(Document::new("note", "scheduling", "mentions the kernel once"))
            .unwrap();
        let graph = KnowledgeGraph::new();
        let retriever = Retriever::new(&store, &graph);
        let hits = retriever.search("kernel", 10);
        assert_eq!(hits.len(), 2);
        // Title match ("kernel design") outranks the body-only mention.
        assert_eq!(hits[0].title, "kernel design");
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
