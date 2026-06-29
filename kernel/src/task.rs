//! Task lifecycle and the state machine from §893.
//!
//! ```text
//! Created -> Queued -> Planning -> Running -> Verifying -> Completed
//!                                     |
//!                                     v
//!                           Failed -> Rollback -> Retry -> Queued
//! ```
//!
//! Transitions are validated centrally so every component agrees on what is
//! legal; illegal transitions return [`KernelError::InvalidTransition`].

use crate::capability::Capability;
use crate::error::{KernelError, Result};
use crate::id::{TaskId, WorkflowId};
use std::fmt;
use std::str::FromStr;

/// The lifecycle state of a task (§893).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskState {
    Created,
    Queued,
    Planning,
    Running,
    Verifying,
    Completed,
    Failed,
    Rollback,
    Retry,
}

impl TaskState {
    /// Stable, human-readable name (used in logs and error messages).
    pub fn name(self) -> &'static str {
        match self {
            TaskState::Created => "Created",
            TaskState::Queued => "Queued",
            TaskState::Planning => "Planning",
            TaskState::Running => "Running",
            TaskState::Verifying => "Verifying",
            TaskState::Completed => "Completed",
            TaskState::Failed => "Failed",
            TaskState::Rollback => "Rollback",
            TaskState::Retry => "Retry",
        }
    }

    /// A terminal state has no outgoing transitions.
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskState::Completed)
    }

    /// Whether a direct transition `self -> next` is permitted.
    pub fn can_transition_to(self, next: TaskState) -> bool {
        use TaskState::*;
        matches!(
            (self, next),
            (Created, Queued)
                | (Queued, Planning)
                | (Queued, Running)
                | (Planning, Running)
                | (Running, Verifying)
                | (Running, Failed)
                | (Verifying, Completed)
                | (Verifying, Failed)
                | (Failed, Rollback)
                | (Rollback, Retry)
                | (Retry, Queued)
        )
    }
}

/// Scheduling priority used by the scheduler's priority queue (§892, §913).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

impl Priority {
    /// Canonical lowercase token.
    pub fn as_token(&self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_token())
    }
}

impl FromStr for Priority {
    type Err = KernelError;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Priority::Low),
            "normal" => Ok(Priority::Normal),
            "high" => Ok(Priority::High),
            "critical" => Ok(Priority::Critical),
            other => Err(KernelError::other(format!("unknown priority: {other}"))),
        }
    }
}

/// A unit of work tracked by the kernel and scheduler.
#[derive(Debug, Clone)]
pub struct Task {
    /// Stable identifier.
    pub id: TaskId,
    /// Optional owning workflow (§895).
    pub workflow: Option<WorkflowId>,
    /// Human-readable description of the intent.
    pub description: String,
    /// Capability required to execute this task (§910, §912).
    pub capability: Capability,
    /// Scheduling priority.
    pub priority: Priority,
    /// Task ids that must reach a terminal `Completed` state first (§892).
    pub depends_on: Vec<TaskId>,
    /// Current lifecycle state.
    state: TaskState,
    /// Number of retry attempts performed so far.
    attempts: u32,
}

impl Task {
    /// Create a new task in the `Created` state.
    pub fn new(description: impl Into<String>, capability: Capability) -> Self {
        Task {
            id: TaskId::new(),
            workflow: None,
            description: description.into(),
            capability,
            priority: Priority::default(),
            depends_on: Vec::new(),
            state: TaskState::Created,
            attempts: 0,
        }
    }

    /// Builder: set priority.
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Builder: declare a dependency.
    pub fn depending_on(mut self, dep: TaskId) -> Self {
        self.depends_on.push(dep);
        self
    }

    /// Builder: attach to a workflow.
    pub fn in_workflow(mut self, wf: WorkflowId) -> Self {
        self.workflow = Some(wf);
        self
    }

    /// Current state.
    pub fn state(&self) -> TaskState {
        self.state
    }

    /// Number of retries attempted.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Attempt a validated state transition (§893).
    ///
    /// Entering `Retry` increments the attempt counter.
    pub fn transition_to(&mut self, next: TaskState) -> Result<()> {
        if !self.state.can_transition_to(next) {
            return Err(KernelError::InvalidTransition {
                from: self.state.name(),
                to: next.name(),
            });
        }
        if next == TaskState::Retry {
            self.attempts += 1;
        }
        self.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_display_fromstr_round_trip() {
        for p in [
            Priority::Low,
            Priority::Normal,
            Priority::High,
            Priority::Critical,
        ] {
            assert_eq!(p.to_string().parse::<Priority>().unwrap(), p);
        }
        assert!("nonsense".parse::<Priority>().is_err());
        // Ordering still holds (used by the scheduler).
        assert!(Priority::Critical > Priority::Low);
    }

    #[test]
    fn happy_path_runs_to_completion() {
        let mut t = Task::new("summarize", Capability::Reasoning);
        for next in [
            TaskState::Queued,
            TaskState::Planning,
            TaskState::Running,
            TaskState::Verifying,
            TaskState::Completed,
        ] {
            t.transition_to(next).expect("legal transition");
        }
        assert!(t.state().is_terminal());
    }

    #[test]
    fn failure_recovers_through_retry() {
        let mut t = Task::new("flaky", Capability::Coding);
        t.transition_to(TaskState::Queued).unwrap();
        t.transition_to(TaskState::Running).unwrap();
        t.transition_to(TaskState::Failed).unwrap();
        t.transition_to(TaskState::Rollback).unwrap();
        t.transition_to(TaskState::Retry).unwrap();
        assert_eq!(t.attempts(), 1);
        t.transition_to(TaskState::Queued).unwrap();
    }

    #[test]
    fn illegal_transition_is_rejected() {
        let mut t = Task::new("x", Capability::Planning);
        let err = t.transition_to(TaskState::Completed).unwrap_err();
        assert!(matches!(err, KernelError::InvalidTransition { .. }));
    }
}
