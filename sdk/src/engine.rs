//! Execution engine — the runnable loop that ties the kernel subsystems
//! together (the orchestration implied by §892–§899).
//!
//! For each task in a workflow the engine:
//! 1. selects a runtime by capability (§924), preferring local backends;
//! 2. runs inference on that runtime (§900);
//! 3. verifies the output on the independent verifier (§899);
//! 4. emits lifecycle events on the bus (§894).
//!
//! Dependency ordering is delegated to the [`Scheduler`](ckos_scheduler::Scheduler)
//! (§892): every task carries its dependency ids (populated by the workflow DAG),
//! so a step only dispatches once its prerequisites complete.
//!
//! The loop is synchronous and dependency-free, keeping the offline-first
//! guarantee (§925); an async/distributed driver (§926) can replace it behind
//! the same surface later.

use ckos_kernel::audit::{AuditRecord, AuditSink, InMemoryAuditLog};
use ckos_kernel::capability::Capability;
use ckos_kernel::error::{KernelError, Result};
use ckos_kernel::event::{Event, EventBus, InMemoryEventBus};
use ckos_kernel::task::{Task, TaskState};
use ckos_kernel::telemetry::{InMemoryTelemetry, TaskMetrics, TelemetrySink};
use ckos_kernel::TaskId;
use ckos_policy::{AccessRequest, PolicyEngine};
use ckos_runtime::{InferenceRequest, RuntimeRegistry};
use ckos_scheduler::{runtime_fit, Scheduler, ScoreFactors};
use ckos_verifier::Verifier;
use ckos_workflow::Dag;
use std::collections::HashMap;
use std::time::Instant;

/// Capabilities whose real-world stakes (regulated advice, physical action)
/// warrant a default-deny authorization check (§929) even though the rest of
/// the capability vocabulary runs unrestricted. Not configurable — deliberately
/// small and fixed, so it can't be quietly narrowed by a careless caller.
///
/// **This gate only fires when a task already carries one of these
/// capabilities.** `ckos_planner::HeuristicPlanner` (the planner behind
/// `ckos run`) deliberately never infers them from free text — see its
/// module doc for why a keyword classifier here was tested and rejected as
/// actively unsafe (it misses most real phrasings, creating false
/// confidence). Concretely: `ckos run --role guest "diagnose my symptoms"`
/// runs completely unauthorized today, because the planner emits `Reasoning`,
/// not `Medical`. The gate only protects a task that explicitly carries the
/// sensitive capability — via a hand-authored `ckos workflow` file
/// (`step x: medical`) or a custom `Planner`/agent that assigns it knowingly.
const SENSITIVE_CAPABILITIES: [Capability; 4] = [
    Capability::Finance,
    Capability::Medical,
    Capability::Legal,
    Capability::Robotics,
];

/// How many times a `Failed` task is recovered through the §893 loop
/// (`Failed → Rollback → Retry → Queued`) before the failure is final.
/// Bounded so a deterministic failure cannot spin forever.
pub const MAX_TASK_RETRIES: u32 = 2;

/// Latency target used to derive `runtime_fit` from observed telemetry when
/// scoring submissions (§904 → §913): runtimes at or under this mean latency
/// are a perfect fit; fit degrades proportionally beyond it.
const TARGET_LATENCY_MS: u64 = 50;

use crate::agent::CapabilityRegistry;
use crate::reflection::{Reflection, Reflector};

/// Outcome of executing a single task.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Task that ran.
    pub task: TaskId,
    /// Capability it required.
    pub capability: Capability,
    /// Name of the agent selected for it, if any was registered (§910).
    pub agent: Option<String>,
    /// Runtime that served it (§900).
    pub runtime: String,
    /// Produced output.
    pub output: String,
    /// Whether the verifier accepted the output (§899).
    pub verified: bool,
    /// The task's final lifecycle state (§893): `Completed` if verified,
    /// `Failed` if the runtime errored or verification failed. Distinguishes
    /// "ran but failed verification" (`Failed` after `Verifying`) from
    /// "never started" (stays `Queued` — see [`Engine::execute`]).
    pub state: TaskState,
}

/// Orchestrates a workflow across runtimes, agents and the verifier.
pub struct Engine {
    runtimes: RuntimeRegistry,
    agents: CapabilityRegistry,
    verifier: Verifier,
    bus: InMemoryEventBus,
    audit: InMemoryAuditLog,
    telemetry: InMemoryTelemetry,
    /// Optional RBAC/ABAC gate (§929) for [`SENSITIVE_CAPABILITIES`]; `None`
    /// (the default) runs every capability unrestricted, matching the
    /// engine's behaviour before this existed. See [`with_policy`](Self::with_policy).
    access: Option<(PolicyEngine, Vec<String>)>,
}

impl Engine {
    /// Build an engine from its subsystems.
    pub fn new(runtimes: RuntimeRegistry, agents: CapabilityRegistry, verifier: Verifier) -> Self {
        Engine {
            runtimes,
            agents,
            verifier,
            bus: InMemoryEventBus::new(),
            audit: InMemoryAuditLog::new(),
            telemetry: InMemoryTelemetry::new(),
            access: None,
        }
    }

    /// Opt in to authorization (§929): tasks whose capability is in
    /// [`SENSITIVE_CAPABILITIES`] (finance/medical/legal/robotics) must be
    /// permitted for `roles` by `policy`, or [`Engine::execute`] denies them
    /// before a runtime is even selected. Ordinary capabilities are never
    /// gated — this narrows the "least privilege" default-deny principle to
    /// where the stakes justify friction, rather than requiring a role for
    /// every task, which would make the common single-operator case unusable.
    pub fn with_policy(mut self, policy: PolicyEngine, roles: Vec<String>) -> Self {
        self.access = Some((policy, roles));
        self
    }

    /// The event bus, so callers can subscribe to execution events (§894).
    pub fn bus(&self) -> &InMemoryEventBus {
        &self.bus
    }

    /// The audit log of executed tasks (§903).
    pub fn audit(&self) -> &InMemoryAuditLog {
        &self.audit
    }

    /// Telemetry aggregated across executed tasks (§904).
    pub fn telemetry(&self) -> &InMemoryTelemetry {
        &self.telemetry
    }

    /// Execute one task: select runtime → run → verify, emitting events and
    /// writing an audit record (§903) on every path, success or failure. Drives
    /// the task through its §893 lifecycle as it actually progresses — a
    /// runtime-selection failure leaves it at `Queued` (it never started); a
    /// runtime error transitions `Running -> Failed`; verification transitions
    /// `Verifying -> Completed` or `Verifying -> Failed`. This is the only
    /// place `TaskState` advances, so `task.state()` always reflects reality.
    pub fn execute(&self, task: &mut Task) -> Result<ExecutionResult> {
        self.bus.publish(Event::TaskStarted(task.id.clone()));
        // A fresh task enters the queue here; a task re-queued through the
        // §893 recovery loop (Failed → Rollback → Retry → Queued, see
        // `run_workflow`) is already Queued and re-enters directly.
        if task.state() == TaskState::Created {
            task.transition_to(TaskState::Queued)?;
        }

        if SENSITIVE_CAPABILITIES.contains(&task.capability) {
            if let Some((policy, roles)) = &self.access {
                let req = AccessRequest {
                    subject: task.id.to_string(),
                    roles: roles.clone(),
                    action: format!("capability.{}", task.capability),
                    attributes: HashMap::new(),
                };
                if let Err(e) = policy.evaluate(&req) {
                    // Denied before a runtime was ever selected — the task
                    // honestly stays Queued (see the doc comment above; there
                    // is no Queued -> Failed edge in the §893 graph).
                    self.bus.publish(Event::PolicyViolation {
                        subject: req.subject.clone(),
                        action: req.action.clone(),
                    });
                    self.audit.record(
                        AuditRecord::new("task.execute")
                            .input(&task.description)
                            .error(e.to_string()),
                    );
                    return Err(e);
                }
            }
        }

        let agent = self
            .agents
            .discover(&task.capability)
            .first()
            .map(|a| a.manifest.id.clone());

        let runtime = match self.runtimes.select(&task.capability) {
            Ok(rt) => rt,
            Err(e) => {
                self.audit.record(
                    AuditRecord::new("task.execute")
                        .input(&task.description)
                        .error(e.to_string()),
                );
                return Err(e);
            }
        };
        let runtime_name = runtime.name().to_string();
        task.transition_to(TaskState::Running)?;

        let started = Instant::now();
        let response = match runtime.run(&InferenceRequest {
            input: task.description.clone(),
            capability: task.capability.clone(),
            max_tokens: 512,
        }) {
            Ok(r) => r,
            Err(e) => {
                task.transition_to(TaskState::Failed)?;
                self.bus.publish(Event::TaskFailed {
                    task: task.id.clone(),
                    reason: e.to_string(),
                });
                self.audit.record(
                    AuditRecord::new("task.execute")
                        .runtime(&runtime_name)
                        .input(&task.description)
                        .error(e.to_string()),
                );
                return Err(e);
            }
        };
        // Record latency/token telemetry for scheduler optimisation (§904, §913).
        self.telemetry.record(TaskMetrics {
            runtime: runtime_name.clone(),
            latency_ms: started.elapsed().as_millis() as u64,
            tokens: response.tokens,
        });

        task.transition_to(TaskState::Verifying)?;
        let report = self.verifier.verify(&response.output);
        let verified = report.passed();
        let mut record = AuditRecord::new("task.execute")
            .runtime(&runtime_name)
            .input(&task.description)
            .output(&response.output);
        if verified {
            task.transition_to(TaskState::Completed)?;
            self.bus.publish(Event::TaskCompleted(task.id.clone()));
        } else {
            task.transition_to(TaskState::Failed)?;
            let reason = report
                .failures()
                .iter()
                .map(|(n, w)| format!("{n}: {w}"))
                .collect::<Vec<_>>()
                .join("; ");
            record = record.error(reason.clone());
            self.bus.publish(Event::TaskFailed {
                task: task.id.clone(),
                reason,
            });
        }
        self.audit.record(record);

        Ok(ExecutionResult {
            task: task.id.clone(),
            capability: task.capability.clone(),
            agent,
            runtime: runtime_name,
            output: response.output,
            verified,
            state: task.state(),
        })
    }

    /// Run a whole workflow in dependency order via the scheduler (§892).
    ///
    /// Tasks are submitted with telemetry-derived scoring (§904 → §913): each
    /// task's `runtime_fit` comes from the observed mean latency of the
    /// runtime that would serve it, so among equally-ready tasks the one on a
    /// faster runtime dispatches first.
    ///
    /// A task that reaches `Failed` (runtime error or verification failure)
    /// is recovered through the §893 loop — `Failed → Rollback → Retry →
    /// Queued` — and re-dispatched, up to [`MAX_TASK_RETRIES`] times. A task
    /// denied before it ever started (policy, no runtime — it stays `Queued`)
    /// is *not* retried: that failure is deterministic.
    ///
    /// Returns results in execution order. Fails fast with
    /// [`KernelError::Other`] if the DAG cannot be ordered (a cycle, or a
    /// dangling/foreign step reference — see [`Dag::topological_order`]), or
    /// propagates a task failure once its retries are exhausted.
    pub fn run_workflow(&self, dag: &Dag) -> Result<Vec<ExecutionResult>> {
        let order = dag.topological_order().ok_or_else(|| {
            KernelError::other("workflow contains a cycle or references an unknown step")
        })?;

        let mut scheduler = Scheduler::new();
        for step in &order {
            if let Some(task) = dag.task(*step) {
                self.submit_scored_by_telemetry(&mut scheduler, task.clone());
            }
        }

        let mut results = Vec::with_capacity(order.len());
        while let Some(mut task) = scheduler.dispatch_next() {
            match self.execute(&mut task) {
                Ok(result) => {
                    // Verification failure is retryable; once the budget is
                    // spent the failed result is reported as-is.
                    if result.state == TaskState::Failed
                        && self.requeue(&mut scheduler, &mut task)?
                    {
                        continue;
                    }
                    scheduler.mark_completed(task.id.clone());
                    results.push(result);
                }
                Err(e) => {
                    // Only a task that actually reached Failed is worth
                    // retrying; a Queued denial (policy, no runtime) is
                    // deterministic. Propagate exhausted/unretryable failures
                    // without publishing WorkflowCompleted — a subscriber
                    // must never observe that event for a workflow that
                    // didn't actually finish.
                    if task.state() == TaskState::Failed
                        && self.requeue(&mut scheduler, &mut task)?
                    {
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        self.bus.publish(Event::WorkflowCompleted(dag.id().clone()));
        Ok(results)
    }

    /// Submit a task scored by observed telemetry (§904 → §913): the runtime
    /// that would serve the task contributes its measured `runtime_fit`. With
    /// no telemetry (or no runtime — the failure then surfaces at execution
    /// time), defaults apply.
    fn submit_scored_by_telemetry(&self, scheduler: &mut Scheduler, task: Task) {
        let factors = match self.runtimes.select(&task.capability) {
            Ok(rt) => self.recommended_factors(rt.name(), TARGET_LATENCY_MS),
            Err(_) => ScoreFactors::default(),
        };
        scheduler.submit_scored(task, factors);
    }

    /// Drive a `Failed` task through the §893 recovery loop and resubmit it,
    /// returning `true` if it was re-queued or `false` if its retry budget
    /// ([`MAX_TASK_RETRIES`]) is exhausted. Entering `Retry` increments the
    /// task's attempt counter (see [`Task::attempts`]).
    fn requeue(&self, scheduler: &mut Scheduler, task: &mut Task) -> Result<bool> {
        if task.attempts() >= MAX_TASK_RETRIES {
            return Ok(false);
        }
        task.transition_to(TaskState::Rollback)?;
        task.transition_to(TaskState::Retry)?;
        task.transition_to(TaskState::Queued)?;
        self.submit_scored_by_telemetry(scheduler, task.clone());
        Ok(true)
    }

    /// Recommend scheduling factors for a runtime from observed telemetry,
    /// closing the §904 → §913 loop: a runtime whose mean latency beats
    /// `target_latency_ms` gets a higher `runtime_fit`, so future tasks on it
    /// score higher. With no telemetry yet, returns optimistic defaults.
    pub fn recommended_factors(&self, runtime: &str, target_latency_ms: u64) -> ScoreFactors {
        let fit = match self.telemetry.mean_latency_for(runtime) {
            Some(latency) => runtime_fit(latency.round() as u64, target_latency_ms),
            None => 1.0,
        };
        ScoreFactors::default().with_runtime_fit(fit)
    }

    /// Self-evaluate a batch of results with the given reflector (§921).
    pub fn reflect(
        &self,
        reflector: &dyn Reflector,
        results: &[ExecutionResult],
    ) -> Vec<Reflection> {
        results.iter().map(|r| reflector.reflect(r)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentManifest;
    use ckos_planner::{HeuristicPlanner, Planner};
    use ckos_runtime::EchoRuntime;
    use ckos_verifier::NonEmptyCheck;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
    fn runs_research_workflow_end_to_end() {
        let engine = research_engine();

        // Count completion events to prove the bus is wired (§894).
        let completed = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&completed);
        engine.bus().subscribe(Arc::new(move |e: &Event| {
            if matches!(e, Event::TaskCompleted(_)) {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }));

        let dag = HeuristicPlanner::new().plan("research the Transformer paper");
        let results = engine.run_workflow(&dag).unwrap();

        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|r| r.verified));
        // The §893 lifecycle actually advanced to Completed for every task —
        // not left dangling at Created/Queued.
        assert!(results.iter().all(|r| r.state == TaskState::Completed));
        // First step is retrieval; it must run before the embedding step.
        assert_eq!(results[0].capability, Capability::Retrieval);
        assert_eq!(completed.load(Ordering::SeqCst), 5);

        // Every step left an audit record, none erroring (§903).
        assert_eq!(engine.audit().len(), 5);
        assert_eq!(engine.audit().error_count(), 0);
        assert!(engine.audit().snapshot().iter().all(|r| r.input_hash != 0));

        // Telemetry captured one sample per step (§904).
        assert_eq!(engine.telemetry().len(), 5);
        assert!(engine.telemetry().total_tokens() > 0);
        assert!(engine.telemetry().mean_latency_for("echo").is_some());

        // Closed loop (§904→§913): the fast echo runtime earns a high
        // runtime_fit; an unseen runtime falls back to the optimistic default.
        let factors = engine.recommended_factors("echo", 100);
        assert!(factors.runtime_fit >= 0.99);
        assert_eq!(engine.recommended_factors("unseen", 100).runtime_fit, 1.0);
    }

    #[test]
    fn workflow_completed_fires_only_after_every_task_actually_ran() {
        let engine = research_engine();
        let completed_events = Arc::new(AtomicUsize::new(0));
        let task_completions_when_wf_completed_fires = Arc::new(AtomicUsize::new(0));
        let tasks_done = Arc::new(AtomicUsize::new(0));

        let done = Arc::clone(&tasks_done);
        engine.bus().subscribe(Arc::new(move |e: &Event| {
            if matches!(e, Event::TaskCompleted(_)) {
                done.fetch_add(1, Ordering::SeqCst);
            }
        }));
        let (wf, snapshot, done2) = (
            Arc::clone(&completed_events),
            Arc::clone(&task_completions_when_wf_completed_fires),
            Arc::clone(&tasks_done),
        );
        engine.bus().subscribe(Arc::new(move |e: &Event| {
            if matches!(e, Event::WorkflowCompleted(_)) {
                wf.fetch_add(1, Ordering::SeqCst);
                // At the moment this fires, every task must already be done —
                // proving the event is not published prematurely.
                snapshot.store(done2.load(Ordering::SeqCst), Ordering::SeqCst);
            }
        }));

        let dag = HeuristicPlanner::new().plan("research the Transformer paper");
        let results = engine.run_workflow(&dag).unwrap();

        assert_eq!(completed_events.load(Ordering::SeqCst), 1);
        assert_eq!(
            task_completions_when_wf_completed_fires.load(Ordering::SeqCst),
            results.len()
        );
    }

    #[test]
    fn execute_drives_the_task_lifecycle_to_completed() {
        let engine = research_engine();
        let mut task = Task::new("summarize", Capability::Reasoning);
        assert_eq!(task.state(), TaskState::Created);
        let result = engine.execute(&mut task).unwrap();
        // Both the task itself and the returned result agree on the final
        // state — this is no longer a parallel, disconnected bookkeeping.
        assert_eq!(task.state(), TaskState::Completed);
        assert_eq!(result.state, TaskState::Completed);
    }

    #[test]
    fn execute_leaves_task_queued_when_no_runtime_is_available() {
        // No runtime registered for the capability: selection fails before the
        // task ever reaches Running. There is no legal Queued -> Failed
        // transition (§893 only allows Failed from Running/Verifying), so the
        // honest state is "never started", not a fabricated failure.
        let engine = Engine::new(
            RuntimeRegistry::new(),
            CapabilityRegistry::new(),
            Verifier::new(),
        );
        let mut task = Task::new("look", Capability::Vision);
        assert!(engine.execute(&mut task).is_err());
        assert_eq!(task.state(), TaskState::Queued);
    }

    #[test]
    fn ordinary_capabilities_run_unrestricted_even_with_a_policy_attached() {
        // Reasoning is not in SENSITIVE_CAPABILITIES, so a deny-everything
        // policy must not block it — only the fixed sensitive set is gated.
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Reasoning])));
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new())
            .with_policy(PolicyEngine::new(), vec!["guest".to_string()]);
        let mut task = Task::new("summarize", Capability::Reasoning);
        assert!(engine.execute(&mut task).is_ok());
    }

    #[test]
    fn sensitive_capability_is_denied_without_a_role_grant() {
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Medical])));
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new())
            .with_policy(PolicyEngine::new(), vec!["guest".to_string()]);

        // A denial is a security-relevant occurrence: it must be observable on
        // the event bus (§894 policy.violation), not only in the audit log.
        let violations = Arc::new(AtomicUsize::new(0));
        let v = Arc::clone(&violations);
        engine.bus().subscribe(Arc::new(move |e: &Event| {
            if matches!(e, Event::PolicyViolation { .. }) {
                v.fetch_add(1, Ordering::SeqCst);
            }
        }));

        let mut task = Task::new("diagnose", Capability::Medical);
        assert!(engine.execute(&mut task).is_err());
        // Denied before a runtime was ever exercised: honestly still Queued.
        assert_eq!(task.state(), TaskState::Queued);
        assert_eq!(violations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sensitive_capability_runs_once_the_role_is_granted() {
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Medical])));
        let mut policy = PolicyEngine::new();
        policy.grant("clinician", "capability.medical");
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new())
            .with_policy(policy, vec!["clinician".to_string()]);
        let mut task = Task::new("diagnose", Capability::Medical);
        let result = engine.execute(&mut task).unwrap();
        assert_eq!(result.state, TaskState::Completed);
    }

    /// A runtime that fails its first `failures` calls, then succeeds —
    /// exercising the §893 recovery loop with a genuinely transient fault.
    struct FlakyRuntime {
        id: ckos_kernel::RuntimeId,
        caps: Vec<Capability>,
        failures: usize,
        calls: AtomicUsize,
    }

    impl FlakyRuntime {
        fn new(cap: Capability, failures: usize) -> Self {
            FlakyRuntime {
                id: ckos_kernel::RuntimeId::new(),
                caps: vec![cap],
                failures,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl ckos_runtime::Runtime for FlakyRuntime {
        fn id(&self) -> &ckos_kernel::RuntimeId {
            &self.id
        }
        fn name(&self) -> &str {
            "flaky"
        }
        fn kind(&self) -> ckos_runtime::RuntimeKind {
            ckos_runtime::RuntimeKind::Cpu
        }
        fn capabilities(&self) -> &[Capability] {
            &self.caps
        }
        fn run(&self, req: &InferenceRequest) -> Result<ckos_runtime::InferenceResponse> {
            if self.calls.fetch_add(1, Ordering::SeqCst) < self.failures {
                return Err(KernelError::other("transient runtime fault"));
            }
            Ok(ckos_runtime::InferenceResponse {
                output: req.input.clone(),
                tokens: 1,
            })
        }
    }

    #[test]
    fn transient_failure_recovers_through_the_893_retry_loop() {
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(FlakyRuntime::new(Capability::Reasoning, 2)));
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new());

        let failed = Arc::new(AtomicUsize::new(0));
        let f = Arc::clone(&failed);
        engine.bus().subscribe(Arc::new(move |e: &Event| {
            if matches!(e, Event::TaskFailed { .. }) {
                f.fetch_add(1, Ordering::SeqCst);
            }
        }));

        let dag = HeuristicPlanner::new().plan("say hello");
        let results = engine.run_workflow(&dag).unwrap();

        // Two transient faults, then success on the final allowed attempt:
        // the workflow completes instead of dying on the first fault.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].state, TaskState::Completed);
        assert_eq!(failed.load(Ordering::SeqCst), 2);
        // Each failed attempt was audited.
        assert_eq!(engine.audit().error_count(), 2);
    }

    #[test]
    fn deterministic_failure_exhausts_the_bounded_retry_budget() {
        let mut runtimes = RuntimeRegistry::new();
        // Fails more times than the budget allows: never recovers.
        runtimes.register(Box::new(FlakyRuntime::new(
            Capability::Reasoning,
            usize::MAX,
        )));
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new());

        let dag = HeuristicPlanner::new().plan("say hello");
        let err = engine.run_workflow(&dag).unwrap_err();
        assert!(err.to_string().contains("transient runtime fault"));
        // 1 initial attempt + MAX_TASK_RETRIES retries, each audited — the
        // loop is bounded, not infinite.
        assert_eq!(engine.audit().error_count(), 1 + MAX_TASK_RETRIES as usize);
    }

    #[test]
    fn observed_latency_orders_dispatch_between_independent_tasks() {
        // §904 → §913 closed loop: pre-record telemetry showing the Vision
        // runtime is slow and the Retrieval runtime fast; of two independent,
        // equal-priority tasks, the fast-runtime one must dispatch first.
        struct NamedEcho {
            id: ckos_kernel::RuntimeId,
            name: &'static str,
            caps: Vec<Capability>,
        }
        impl ckos_runtime::Runtime for NamedEcho {
            fn id(&self) -> &ckos_kernel::RuntimeId {
                &self.id
            }
            fn name(&self) -> &str {
                self.name
            }
            fn kind(&self) -> ckos_runtime::RuntimeKind {
                ckos_runtime::RuntimeKind::Cpu
            }
            fn capabilities(&self) -> &[Capability] {
                &self.caps
            }
            fn run(&self, req: &InferenceRequest) -> Result<ckos_runtime::InferenceResponse> {
                Ok(ckos_runtime::InferenceResponse {
                    output: req.input.clone(),
                    tokens: 1,
                })
            }
        }
        let mut runtimes = RuntimeRegistry::new();
        for (name, cap) in [
            ("slow-rt", Capability::Vision),
            ("fast-rt", Capability::Retrieval),
        ] {
            runtimes.register(Box::new(NamedEcho {
                id: ckos_kernel::RuntimeId::new(),
                name,
                caps: vec![cap],
            }));
        }
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new());
        for (rt, latency) in [("slow-rt", 500), ("fast-rt", 1)] {
            engine.telemetry().record(TaskMetrics {
                runtime: rt.into(),
                latency_ms: latency,
                tokens: 1,
            });
        }

        // Two independent steps; the slow-runtime one is added first, so a
        // regression to default (tied) scoring would dispatch it first.
        let mut dag = Dag::new("mixed");
        dag.add_step(Task::new("look", Capability::Vision), &[]);
        dag.add_step(Task::new("fetch", Capability::Retrieval), &[]);
        let results = engine.run_workflow(&dag).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].capability,
            Capability::Retrieval,
            "telemetry-scored submission must dispatch the faster runtime's task first"
        );
    }

    #[test]
    fn missing_runtime_fails_the_task() {
        // Engine with no runtime registered for the required capability.
        let engine = Engine::new(
            RuntimeRegistry::new(),
            CapabilityRegistry::new(),
            Verifier::new(),
        );
        let completed = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&completed);
        engine.bus().subscribe(Arc::new(move |e: &Event| {
            if matches!(e, Event::WorkflowCompleted(_)) {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }));

        let dag = HeuristicPlanner::new().plan("say hello");
        let err = engine.run_workflow(&dag).unwrap_err();
        assert!(matches!(err, KernelError::CapabilityUnavailable(_)));
        // The failure was still audited.
        assert_eq!(engine.audit().error_count(), 1);
        // A workflow that failed must never publish WorkflowCompleted.
        assert_eq!(completed.load(Ordering::SeqCst), 0);
    }
}
