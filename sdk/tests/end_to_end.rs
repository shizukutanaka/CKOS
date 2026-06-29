//! End-to-end integration test driving the whole CKOS pipeline through the
//! public `ckos_sdk::prelude` — the same surface an application would use.
//!
//! It exercises: plan → schedule → execute → verify (engine), reflection,
//! durable sessions + resume, hybrid retrieval, audit + telemetry, the
//! telemetry→scheduler loop, KQL over the graph, the knowledge-bus auto-reindex,
//! identity→policy authorization, signed messaging, and graph versioning.

use ckos_sdk::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ckos-e2e-{}-{n}", std::process::id()));
        TempDir(dir)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn research_engine() -> Engine {
    let mut runtimes = RuntimeRegistry::new();
    let mut agents = CapabilityRegistry::new();
    for cap in [
        Capability::Retrieval,
        Capability::Embedding,
        Capability::Reasoning,
        Capability::Verification,
    ] {
        runtimes.register(Box::new(EchoRuntime::new(vec![cap.clone()])));
        agents.register(AgentManifest::new(format!("{cap}-agent"), cap));
    }
    let verifier = Verifier::new().with_check(Box::new(NonEmptyCheck));
    Engine::new(runtimes, agents, verifier)
}

#[test]
fn full_pipeline_plan_execute_reflect_persist_retrieve() {
    let engine = research_engine();

    // Plan → execute the research workflow (§895, §898, §892, §899).
    let dag = HeuristicPlanner::new().plan("research the Transformer paper");
    let results = engine.run_workflow(&dag).unwrap();
    assert_eq!(results.len(), 5);
    assert!(results.iter().all(|r| r.verified));

    // Observability populated (§903, §904).
    assert_eq!(engine.audit().len(), 5);
    assert_eq!(engine.audit().error_count(), 0);
    assert_eq!(engine.telemetry().len(), 5);

    // Reflection + consensus (§921, §922).
    let reflections = engine.reflect(&HeuristicReflector::new(), &results);
    assert!(consensus(&reflections).score > 0);

    // Telemetry → scheduler loop (§904 → §913).
    assert!(engine.recommended_factors("echo", 100).runtime_fit >= 0.99);

    // Persist a durable session and resume it in a fresh store (§927, §956).
    let tmp = TempDir::new();
    {
        let store = FileStore::open(&tmp.0).unwrap();
        let mut session = Session::new("e2e", Box::new(store))
            .with_embedder(Box::new(HashingEmbedder::default()));
        session.record_run(&results).unwrap();
        session.record_reflections(&reflections).unwrap();
    }
    let reopened = Session::new("e2e", Box::new(FileStore::open(&tmp.0).unwrap()));
    assert_eq!(reopened.history().unwrap().len(), 5);

    // Hybrid retrieval over the persisted, embedded documents (§949, §950).
    let store = FileStore::open(&tmp.0).unwrap();
    let graph = KnowledgeGraph::new();
    let embedder = HashingEmbedder::default();
    let retriever = Retriever::with_embedder(&store, &graph, &embedder);
    assert!(!retriever.search("generate report summary", 10).is_empty());
}

#[test]
fn kql_knowledge_bus_security_policy_versioning() {
    // KQL over a demo graph (§962).
    let mut graph = KnowledgeGraph::new();
    let t = graph.add_node(NodeKind::Concept, "Transformer", 96);
    let a = graph.add_node(NodeKind::Other("algorithm".into()), "Attention", 92);
    graph.connect(&t, &a, EdgeKind::References);
    let q = kql_parse("FIND Concept \"Transformer\" RELATED Algorithm FILTER Confidence >= 90")
        .unwrap();
    let res = kql_execute(&q, &graph);
    assert_eq!(res.primary.len(), 1);
    assert_eq!(res.related.len(), 1);

    // Knowledge bus → auto-reindex into a vector store (§923 → §938).
    let mut kb = KnowledgeBus::new();
    let queue = kb.subscribe_reindex();
    kb.add_node(NodeKind::Concept, "Embeddings", 88);
    let embedder = HashingEmbedder::new(64);
    let mut store = InMemoryStore::new();
    assert_eq!(
        Reindexer::new(kb.graph(), &embedder).process(&queue, &mut store),
        1
    );

    // Identity → policy authorization (§928, §929).
    let mut provider = StaticTokenProvider::new();
    provider.add_token("tok", Identity::new("alice").with_role("editor"));
    let mut policy = PolicyEngine::new();
    policy.grant("editor", "graph.write");
    let id = provider.authenticate("tok").unwrap();
    assert!(policy.evaluate(&id.request("graph.write")).is_ok());
    assert!(policy.evaluate(&id.request("docker.run")).is_err());

    // Signed messaging with replay protection (§915, §930).
    let signer = Signer::new(42);
    let mut guard = ReplayGuard::new(42, 1000);
    let msg = Message::new(AgentId::new(), AgentId::new(), "reasoning").with_body("hi");
    let env = signer.seal(msg, 1, 10_000);
    assert!(guard.verify(&env, 10_100).is_ok());
    assert_eq!(guard.verify(&env, 10_200), Err(SecurityError::Replayed));

    // Graph versioning: branch, diverge, merge (§942, §943).
    let mut repo = GraphRepo::new();
    let mut main = KnowledgeGraph::new();
    main.add_node(NodeKind::Concept, "A", 60);
    repo.commit(main);
    repo.branch("feature");
    repo.checkout("feature");
    let mut feat = KnowledgeGraph::new();
    feat.add_node(NodeKind::Concept, "A", 95);
    feat.add_node(NodeKind::Concept, "B", 80);
    repo.commit(feat);
    repo.checkout("main");
    let report = repo
        .merge("feature", MergeStrategy::HigherConfidence)
        .unwrap();
    assert_eq!(repo.head().len(), 2);
    assert_eq!(report.conflicts.len(), 1);
}
