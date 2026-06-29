//! Kernel error taxonomy.
//!
//! The kernel never performs inference (§891); its errors are therefore about
//! orchestration: invalid state transitions, missing resources, policy
//! violations and capacity limits.

use std::fmt;

/// Result alias used throughout the kernel.
pub type Result<T> = std::result::Result<T, KernelError>;

/// Errors surfaced by kernel-level operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    /// A task lifecycle transition was not permitted (§893).
    InvalidTransition {
        /// State the task was in.
        from: &'static str,
        /// State that was requested.
        to: &'static str,
    },
    /// A referenced entity (task, agent, runtime, …) does not exist.
    NotFound(String),
    /// An action was denied by policy (§929) — surfaced as `PolicyViolation`.
    PolicyDenied(String),
    /// A required capability could not be satisfied by any agent (§910).
    CapabilityUnavailable(String),
    /// A resource quota or scheduler queue limit was exceeded (§891, §913).
    ResourceExhausted(String),
    /// Generic, caller-supplied failure with a message.
    Other(String),
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KernelError::InvalidTransition { from, to } => {
                write!(f, "invalid task transition: {from} -> {to}")
            }
            KernelError::NotFound(what) => write!(f, "not found: {what}"),
            KernelError::PolicyDenied(why) => write!(f, "policy denied: {why}"),
            KernelError::CapabilityUnavailable(cap) => {
                write!(f, "no agent provides capability: {cap}")
            }
            KernelError::ResourceExhausted(what) => write!(f, "resource exhausted: {what}"),
            KernelError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for KernelError {}

impl KernelError {
    /// Convenience constructor for ad-hoc errors.
    pub fn other(msg: impl Into<String>) -> Self {
        KernelError::Other(msg.into())
    }
}
