//! # CKOS Scheduler
//!
//! The scheduler is split into four layers (§892):
//!
//! ```text
//! Task Queue -> Priority Queue -> Dependency Resolver -> Execution Dispatcher
//! ```
//!
//! - **Task Queue** — ingestion buffer for newly created tasks.
//! - **Priority Queue** — orders ready tasks by a multi-factor score (§913):
//!   deadline, importance, cost, runtime fit, energy and confidence.
//! - **Dependency Resolver** — holds back tasks whose dependencies are unmet
//!   (§892), releasing them once prerequisites complete.
//! - **Execution Dispatcher** — hands the next runnable task to the kernel.

use ckos_kernel::task::{Priority, Task};
use ckos_kernel::TaskId;
use std::collections::HashSet;

/// Multi-factor scoring inputs used by the priority queue (§913).
///
/// All factors are normalised to `0.0..=1.0`. Higher `score()` runs sooner.
#[derive(Debug, Clone, Copy)]
pub struct ScoreFactors {
    /// Urgency from an approaching deadline (1.0 = overdue).
    pub deadline: f32,
    /// Business/user importance.
    pub importance: f32,
    /// Inverse cost (1.0 = cheap). Cheap work is preferred when otherwise equal.
    pub cost_efficiency: f32,
    /// How well a suitable runtime is currently available (1.0 = warm & idle).
    pub runtime_fit: f32,
    /// Energy efficiency (1.0 = low power). Relevant on edge devices (§904).
    pub energy: f32,
    /// Planner confidence the task will succeed.
    pub confidence: f32,
}

impl Default for ScoreFactors {
    fn default() -> Self {
        ScoreFactors {
            deadline: 0.0,
            importance: 0.5,
            cost_efficiency: 0.5,
            runtime_fit: 0.5,
            energy: 0.5,
            confidence: 0.5,
        }
    }
}

impl ScoreFactors {
    /// Weighted blend of the factors. Weights favour deadline and importance,
    /// matching the spec's ordering in §913.
    pub fn score(&self, base: Priority) -> f32 {
        let base_weight = match base {
            Priority::Low => 0.0,
            Priority::Normal => 0.25,
            Priority::High => 0.5,
            Priority::Critical => 1.0,
        };
        base_weight
            + 0.30 * self.deadline
            + 0.25 * self.importance
            + 0.15 * self.runtime_fit
            + 0.10 * self.confidence
            + 0.10 * self.cost_efficiency
            + 0.10 * self.energy
    }
}

/// A task plus its computed scheduling score.
struct Pending {
    task: Task,
    factors: ScoreFactors,
}

/// The four-layer scheduler (§892).
#[derive(Default)]
pub struct Scheduler {
    /// Layer 1+2: ingested tasks awaiting dependency resolution, scored.
    pending: Vec<Pending>,
    /// Tasks already dispatched/completed — used by the dependency resolver.
    completed: HashSet<TaskId>,
}

impl Scheduler {
    /// Create an empty scheduler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Layer 1 — submit a task with default scoring.
    pub fn submit(&mut self, task: Task) {
        self.submit_scored(task, ScoreFactors::default());
    }

    /// Layer 1 — submit a task with explicit scoring factors (§913).
    pub fn submit_scored(&mut self, task: Task, factors: ScoreFactors) {
        self.pending.push(Pending { task, factors });
    }

    /// Number of tasks awaiting dispatch.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Mark a task complete so dependents become eligible (Layer 3).
    pub fn mark_completed(&mut self, id: TaskId) {
        self.completed.insert(id);
    }

    /// Whether every dependency of `task` has completed (Layer 3).
    fn dependencies_met(&self, task: &Task) -> bool {
        task.depends_on.iter().all(|d| self.completed.contains(d))
    }

    /// Layer 3+4 — return the highest-scoring task whose dependencies are met,
    /// removing it from the queue. `None` if nothing is runnable yet.
    pub fn dispatch_next(&mut self) -> Option<Task> {
        let mut best: Option<usize> = None;
        let mut best_score = f32::MIN;
        for (i, p) in self.pending.iter().enumerate() {
            if !self.dependencies_met(&p.task) {
                continue;
            }
            let s = p.factors.score(p.task.priority);
            if s > best_score {
                best_score = s;
                best = Some(i);
            }
        }
        best.map(|i| self.pending.swap_remove(i).task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckos_kernel::Capability;

    #[test]
    fn higher_priority_dispatches_first() {
        let mut s = Scheduler::new();
        s.submit(Task::new("low", Capability::Reasoning).with_priority(Priority::Low));
        s.submit(Task::new("crit", Capability::Reasoning).with_priority(Priority::Critical));
        let first = s.dispatch_next().unwrap();
        assert_eq!(first.description, "crit");
    }

    #[test]
    fn dependencies_gate_dispatch() {
        let mut s = Scheduler::new();
        let a = Task::new("a", Capability::Coding);
        let a_id = a.id.clone();
        let b = Task::new("b", Capability::Coding).depending_on(a_id.clone());
        s.submit(b);
        // b cannot run until a completes.
        assert!(s.dispatch_next().is_none());
        s.mark_completed(a_id);
        assert_eq!(s.dispatch_next().unwrap().description, "b");
    }
}
