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

/// Map an observed runtime latency to a `runtime_fit` factor in `0.0..=1.0`
/// (closing the telemetry → scheduler loop, §904 → §913).
///
/// At or below `target_latency_ms` the runtime is a perfect fit (1.0); fit
/// degrades proportionally as observed latency exceeds the target. An unknown
/// (zero) latency optimistically returns 1.0.
pub fn runtime_fit(observed_latency_ms: u64, target_latency_ms: u64) -> f32 {
    if observed_latency_ms == 0 {
        return 1.0;
    }
    let target = target_latency_ms.max(1) as f32;
    (target / observed_latency_ms as f32).min(1.0)
}

impl ScoreFactors {
    /// Builder: set `runtime_fit` (clamped to `0.0..=1.0`), e.g. from
    /// [`runtime_fit`] applied to observed telemetry.
    pub fn with_runtime_fit(mut self, fit: f32) -> Self {
        self.runtime_fit = fit.clamp(0.0, 1.0);
        self
    }

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

/// A task plus its computed scheduling score and the clock tick it was enqueued
/// at (for aging).
struct Pending {
    task: Task,
    factors: ScoreFactors,
    enqueued_at: u64,
}

/// Default aging rate: how much a task's effective score grows per dispatch
/// cycle it waits. Small but enough that a starved task eventually wins.
const DEFAULT_AGING_RATE: f32 = 0.05;

/// The four-layer scheduler (§892).
pub struct Scheduler {
    /// Layer 1+2: ingested tasks awaiting dependency resolution, scored.
    pending: Vec<Pending>,
    /// Tasks already dispatched/completed — used by the dependency resolver.
    completed: HashSet<TaskId>,
    /// Monotonic dispatch-cycle counter, the basis for aging.
    clock: u64,
    /// Score added per cycle a task has waited (anti-starvation, §892).
    aging_rate: f32,
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler {
            pending: Vec::new(),
            completed: HashSet::new(),
            clock: 0,
            aging_rate: DEFAULT_AGING_RATE,
        }
    }
}

impl Scheduler {
    /// Create an empty scheduler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the aging rate — the score a waiting task gains per dispatch cycle.
    /// Higher values reclaim starved tasks sooner; 0 disables aging.
    pub fn with_aging_rate(mut self, rate: f32) -> Self {
        self.aging_rate = rate.max(0.0);
        self
    }

    /// Layer 1 — submit a task with default scoring.
    pub fn submit(&mut self, task: Task) {
        self.submit_scored(task, ScoreFactors::default());
    }

    /// Layer 1 — submit a task with explicit scoring factors (§913).
    pub fn submit_scored(&mut self, task: Task, factors: ScoreFactors) {
        self.pending.push(Pending {
            task,
            factors,
            enqueued_at: self.clock,
        });
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
    ///
    /// Effective score includes an **aging** term proportional to how long the
    /// task has waited, so a low-priority task cannot be starved indefinitely by
    /// a stream of higher-priority arrivals (§892).
    pub fn dispatch_next(&mut self) -> Option<Task> {
        self.clock += 1;
        let mut best: Option<usize> = None;
        let mut best_score = f32::MIN;
        for (i, p) in self.pending.iter().enumerate() {
            if !self.dependencies_met(&p.task) {
                continue;
            }
            let age = self.clock.saturating_sub(p.enqueued_at) as f32;
            let s = p.factors.score(p.task.priority) + self.aging_rate * age;
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
    fn latency_maps_to_runtime_fit() {
        assert_eq!(runtime_fit(0, 100), 1.0); // unknown → optimistic
        assert_eq!(runtime_fit(50, 100), 1.0); // faster than target → perfect
        assert_eq!(runtime_fit(200, 100), 0.5); // 2x slower → half fit
    }

    #[test]
    fn faster_runtime_dispatches_first_via_telemetry_fit() {
        // Two equal-priority tasks; the one on the faster runtime (higher
        // runtime_fit from observed latency) should dispatch first (§904→§913).
        let mut s = Scheduler::new();
        s.submit_scored(
            Task::new("slow-runtime", Capability::Reasoning),
            ScoreFactors::default().with_runtime_fit(runtime_fit(400, 100)),
        );
        s.submit_scored(
            Task::new("fast-runtime", Capability::Reasoning),
            ScoreFactors::default().with_runtime_fit(runtime_fit(80, 100)),
        );
        assert_eq!(s.dispatch_next().unwrap().description, "fast-runtime");
    }

    #[test]
    fn aging_prevents_starvation() {
        // A single low-priority task amid a stream of Critical arrivals must
        // eventually dispatch rather than starve forever.
        let mut s = Scheduler::new().with_aging_rate(0.1);
        s.submit(Task::new("low", Capability::Reasoning).with_priority(Priority::Low));
        let mut dispatched_low = false;
        for _ in 0..30 {
            s.submit(Task::new("crit", Capability::Reasoning).with_priority(Priority::Critical));
            if s.dispatch_next().unwrap().description == "low" {
                dispatched_low = true;
                break;
            }
        }
        assert!(
            dispatched_low,
            "low-priority task must not starve under aging"
        );
    }

    #[test]
    fn aging_disabled_lets_priority_dominate() {
        // With aging off, the low task never beats fresh Critical arrivals.
        let mut s = Scheduler::new().with_aging_rate(0.0);
        s.submit(Task::new("low", Capability::Reasoning).with_priority(Priority::Low));
        for _ in 0..5 {
            s.submit(Task::new("crit", Capability::Reasoning).with_priority(Priority::Critical));
            assert_eq!(s.dispatch_next().unwrap().description, "crit");
        }
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
