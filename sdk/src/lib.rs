//! # CKOS SDK
//!
//! The developer-facing entry point. It re-exports the kernel subsystems
//! through a single [`prelude`] and adds the agent layer (§907–§910):
//! manifests, lifecycle and capability-based discovery.
//!
//! ```
//! use ckos_sdk::prelude::*;
//!
//! // Build a research plan and confirm it is schedulable.
//! let dag = HeuristicPlanner::new().plan("research the Transformer paper");
//! assert!(dag.topological_order().is_some());
//!
//! // Discover an agent by capability, not by name.
//! let mut registry = CapabilityRegistry::new();
//! registry.register(AgentManifest::new("summarizer", Capability::Reasoning));
//! assert_eq!(registry.discover(&Capability::Reasoning).len(), 1);
//! ```

pub mod agent;
pub mod crypto;
pub mod engine;
pub mod eval;
pub mod knowledge_bus;
pub mod kql;
pub mod messaging;
pub mod reflection;
pub mod retrieval;
pub mod security;
pub mod session;
pub mod synonyms;

pub use agent::{AgentInstance, AgentManifest, AgentState, CapabilityRegistry};
pub use engine::{Engine, ExecutionResult};
pub use eval::{
    average_precision, evaluate, evaluate_hits, mean_average_precision, mean_reciprocal_rank,
    EvalScores,
};
pub use knowledge_bus::{KnowledgeBus, ReindexQueue, Reindexer};
pub use kql::{execute as kql_execute, parse as kql_parse, KqlQuery, KqlResult};
pub use messaging::{Message, MessageBus, Payload, ServiceMesh};
pub use reflection::{consensus, Consensus, HeuristicReflector, Reflection, Reflector};
pub use retrieval::{
    expand_query, mmr_rerank, plan_retrieval, Hit, HitSource, RetrievalStrategy, Retriever,
    SearchCache,
};
pub use security::{sign, ReplayGuard, SecurityError, SignedEnvelope, Signer};
pub use session::Session;
pub use synonyms::{expand_query_with_synonyms, SynonymTable};

/// One-stop import surface for applications building on CKOS.
pub mod prelude {
    pub use crate::agent::{AgentInstance, AgentManifest, AgentState, CapabilityRegistry};
    pub use crate::engine::{Engine, ExecutionResult};
    pub use crate::eval::{
        average_precision, evaluate, evaluate_hits, mean_average_precision, mean_reciprocal_rank,
        EvalScores,
    };
    pub use crate::knowledge_bus::{KnowledgeBus, ReindexQueue, Reindexer};
    pub use crate::kql::{
        execute as kql_execute, parse as kql_parse, KqlQuery, KqlResult, NodeMatch, ReturnTarget,
        SortDir,
    };
    pub use crate::messaging::{Message, MessageBus, Payload, ServiceMesh};
    pub use crate::reflection::{consensus, Consensus, HeuristicReflector, Reflection, Reflector};
    pub use crate::retrieval::{
        expand_query, mmr_rerank, plan_retrieval, Hit, HitSource, RetrievalStrategy, Retriever,
        SearchCache,
    };
    pub use crate::security::{sign, ReplayGuard, SecurityError, SignedEnvelope, Signer};
    pub use crate::session::Session;
    pub use crate::synonyms::{expand_query_with_synonyms, SynonymTable};

    pub use ckos_kernel::audit::{AuditRecord, AuditSink, InMemoryAuditLog};
    pub use ckos_kernel::capability::Capability;
    pub use ckos_kernel::error::{KernelError, Result};
    pub use ckos_kernel::event::{Event, EventBus, InMemoryEventBus};
    pub use ckos_kernel::task::{Priority, Task, TaskState};
    pub use ckos_kernel::telemetry::{InMemoryTelemetry, TaskMetrics, TelemetrySink};
    pub use ckos_kernel::{AgentId, DocumentId, NodeId, RuntimeId, TaskId, WorkflowId};

    pub use ckos_scheduler::{runtime_fit, Scheduler, ScoreFactors};

    pub use ckos_runtime::{
        EchoRuntime, InferenceRequest, InferenceResponse, Runtime, RuntimeInfo, RuntimeKind,
        RuntimeRegistry,
    };

    pub use ckos_graph::{
        EdgeKind, GraphRepo, GraphStore, KnowledgeGraph, MergeConflict, MergeReport, MergeStrategy,
        Node, NodeKind, VersionId,
    };

    pub use ckos_memory::{
        chunk, chunk_with_overlap, collect as gc_collect, compress_document, consolidate, cosine,
        keywords, rank_memories, recency_decay, summarize, ChunkStrategy, Document, Embedder,
        FileStore, GcPolicy, GcReason, GcReport, HashingEmbedder, InMemoryStore, MemorySignals,
        MemoryWeights, Query, Storage,
    };

    pub use ckos_planner::{HeuristicPlanner, Planner, SubTask};

    pub use ckos_verifier::{
        ArithmeticCheck, Check, CitationCheck, ForbiddenContentCheck, JsonBalanceCheck,
        NonEmptyCheck, RepetitionCheck, Report, Verdict, Verifier,
    };

    pub use ckos_policy::{
        AbacRule, AccessRequest, Identity, IdentityProvider, PolicyEngine, StaticTokenProvider,
    };

    pub use ckos_workflow::{Dag, StepRef};

    pub use ckos_plugins::{Tool, ToolMetadata, ToolRegistry, UppercaseTool};
}
