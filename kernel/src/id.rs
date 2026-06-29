//! Typed, process-unique identifiers.
//!
//! IDs are monotonic within a process and prefixed by their domain so they are
//! self-describing in logs and audit trails (§903). They are intentionally
//! dependency-free: a global atomic counter mixed with a coarse timestamp keeps
//! them unique without pulling in a UUID crate.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Generate a new opaque id string for the given domain prefix, e.g. `task`.
fn mint(prefix: &str) -> String {
    format!("{prefix}-{:x}-{:x}", epoch_millis(), next_seq())
}

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Mint a fresh, process-unique id.
            pub fn new() -> Self {
                Self(mint($prefix))
            }

            /// Wrap an existing string (e.g. when rehydrating from storage).
            pub fn from_raw(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            /// Borrow the underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
    };
}

typed_id!(
    /// Identifier for a unit of work tracked by the scheduler (§892).
    TaskId, "task"
);
typed_id!(
    /// Identifier for an agent/service instance (§907–§909).
    AgentId, "agent"
);
typed_id!(
    /// Identifier for a workflow DAG instance (§895).
    WorkflowId, "wf"
);
typed_id!(
    /// Identifier for a node inside the knowledge graph (§897).
    NodeId, "node"
);
typed_id!(
    /// Identifier for a registered runtime backend (§900).
    RuntimeId, "rt"
);
typed_id!(
    /// Identifier for a stored document / memory record (§937).
    DocumentId, "doc"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_prefixed() {
        let a = TaskId::new();
        let b = TaskId::new();
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("task-"));
    }

    #[test]
    fn round_trips_through_raw() {
        let id = AgentId::from_raw("agent-fixed");
        assert_eq!(id.as_str(), "agent-fixed");
    }
}
