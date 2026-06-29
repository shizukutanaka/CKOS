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
pub mod engine;
pub mod kql;
pub mod reflection;
pub mod retrieval;
pub mod session;

pub use agent::{AgentInstance, AgentManifest, AgentState, CapabilityRegistry};
pub use engine::{Engine, ExecutionResult};
pub use kql::{execute as kql_execute, parse as kql_parse, KqlQuery, KqlResult};
pub use reflection::{consensus, Consensus, HeuristicReflector, Reflection, Reflector};
pub use retrieval::{plan_retrieval, Hit, HitSource, RetrievalStrategy, Retriever};
pub use session::Session;

/// One-stop import surface for applications building on CKOS.
pub mod prelude {
    pub use crate::agent::{AgentInstance, AgentManifest, AgentState, CapabilityRegistry};
    pub use crate::engine::{Engine, ExecutionResult};
    pub use crate::kql::{
        execute as kql_execute, parse as kql_parse, KqlQuery, KqlResult, NodeMatch, ReturnTarget,
    };
    pub use crate::reflection::{consensus, Consensus, HeuristicReflector, Reflection, Reflector};
    pub use crate::retrieval::{plan_retrieval, Hit, HitSource, RetrievalStrategy, Retriever};
    pub use crate::session::Session;

    pub use ckos_kernel::audit::{AuditRecord, AuditSink, InMemoryAuditLog};
    pub use ckos_kernel::capability::Capability;
    pub use ckos_kernel::error::{KernelError, Result};
    pub use ckos_kernel::event::{Event, EventBus, InMemoryEventBus};
    pub use ckos_kernel::task::{Priority, Task, TaskState};
    pub use ckos_kernel::{AgentId, DocumentId, NodeId, RuntimeId, TaskId, WorkflowId};

    pub use ckos_scheduler::{Scheduler, ScoreFactors};

    pub use ckos_runtime::{
        EchoRuntime, InferenceRequest, InferenceResponse, Runtime, RuntimeKind, RuntimeRegistry,
    };

    pub use ckos_graph::{EdgeKind, KnowledgeGraph, Node, NodeKind};

    pub use ckos_memory::{
        collect as gc_collect, compress_document, cosine, summarize, Document, Embedder, FileStore,
        GcPolicy, GcReason, GcReport, HashingEmbedder, InMemoryStore, MemoryTier, Query, Storage,
    };

    pub use ckos_planner::{HeuristicPlanner, Planner, SubTask};

    pub use ckos_verifier::{Check, JsonBalanceCheck, NonEmptyCheck, Report, Verdict, Verifier};

    pub use ckos_policy::{AbacRule, AccessRequest, PolicyEngine};

    pub use ckos_workflow::{Dag, StepRef};

    pub use ckos_plugins::{PluginKind, Tool, ToolMetadata, ToolRegistry, UppercaseTool};
}
