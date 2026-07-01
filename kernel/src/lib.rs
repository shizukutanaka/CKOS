//! # CKOS Kernel
//!
//! The Cognitive Kernel is the heart of CKOS. Per §891 it **does not perform
//! inference**; its responsibilities are limited to orchestration:
//!
//! - task management & the lifecycle state machine ([`task`])
//! - capabilities, the vocabulary agents are matched by ([`capability`])
//! - the event bus for loosely-coupled module communication ([`event`])
//! - audit logging, kept separate from debug logging ([`audit`])
//! - telemetry feeding scheduler optimisation ([`telemetry`])
//! - typed identifiers ([`id`])
//! - a shared error taxonomy ([`error`])
//!
//! Keeping inference out of the kernel means a change of runtime
//! (llama.cpp → vLLM → MLX …) never ripples into the kernel — exactly the
//! decoupling the spec calls for.
//!
//! See the workspace `docs/` directory for the full v2.5–v2.7 design.

pub mod audit;
pub mod capability;
pub mod error;
pub mod event;
pub mod id;
pub mod permission;
pub mod task;
pub mod telemetry;

pub use audit::{AuditRecord, AuditSink, InMemoryAuditLog};
pub use capability::Capability;
pub use error::{KernelError, Result};
pub use event::{Event, EventBus, InMemoryEventBus};
pub use id::{AgentId, DocumentId, NodeId, RuntimeId, TaskId, WorkflowId};
pub use permission::permission_matches;
pub use task::{Priority, Task, TaskState};
pub use telemetry::{
    InMemoryTelemetry, NullProbe, ResourceProbe, ResourceSnapshot, TaskMetrics, TelemetrySink,
};
