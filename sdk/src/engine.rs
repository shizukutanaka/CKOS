//! Execution engine — the runnable loop that ties the kernel subsystems
//! together (the orchestration implied by §892–§899).
//!
//! For each task in a workflow the engine:
//! 1. selects a runtime by capability (§924), preferring local backends;
//! 2. runs inference on that runtime (§900);
//! 3. verifies the output on the independent verifier (§899);
//! 4. emits lifecycle events on the bus (§894).
//!
//! Dependency ordering is delegated to the [`Scheduler`]
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
use ckos_kernel::task::{Priority, Task, TaskState};
use ckos_kernel::telemetry::{InMemoryTelemetry, TaskMetrics, TelemetrySink};
use ckos_kernel::TaskId;
use ckos_policy::{Identity, PolicyEngine};
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
    /// engine's behaviour before this existed. See [`with_policy`](Self::with_policy)
    /// and [`with_identity`](Self::with_identity).
    access: Option<(PolicyEngine, Identity)>,
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
    /// `SENSITIVE_CAPABILITIES` (finance/medical/legal/robotics) must be
    /// permitted for `roles` by `policy`, or [`Engine::execute`] denies them
    /// before a runtime is even selected. Ordinary capabilities are never
    /// gated — this narrows the "least privilege" default-deny principle to
    /// where the stakes justify friction, rather than requiring a role for
    /// every task, which would make the common single-operator case unusable.
    ///
    /// A convenience over [`with_identity`](Self::with_identity) for callers
    /// that only have bare roles (no authenticated identity/attributes) —
    /// e.g. a hardcoded `--role` flag. ABAC rules that key off attributes
    /// (§929) never match through this path, since there are none to carry;
    /// use `with_identity` with a real [`Identity`] (e.g. from an
    /// [`ckos_policy::IdentityProvider`]) to exercise ABAC.
    pub fn with_policy(self, policy: PolicyEngine, roles: Vec<String>) -> Self {
        self.with_identity(
            policy,
            Identity {
                subject: "role-grant".into(),
                roles,
                attributes: HashMap::new(),
            },
        )
    }

    /// Opt in to authorization (§929) with a full [`Identity`] — subject,
    /// roles *and* ABAC attributes — typically obtained by authenticating a
    /// credential through an [`ckos_policy::IdentityProvider`]. Unlike
    /// [`with_policy`](Self::with_policy), this lets ABAC rules (e.g. a
    /// region- or clearance-based deny) actually evaluate against real
    /// attribute values.
    pub fn with_identity(mut self, policy: PolicyEngine, identity: Identity) -> Self {
        self.access = Some((policy, identity));
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

    /// Execute one task: select runtime → run → verify, writing an audit
    /// record (§903) on every path, success or failure. Drives the task
    /// through its §893 lifecycle as it actually progresses — a
    /// runtime-selection failure leaves it at `Queued` (it never started); a
    /// runtime error transitions `Running -> Failed`; verification transitions
    /// `Verifying -> Completed` or `Verifying -> Failed`. This is the only
    /// place `TaskState` advances, so `task.state()` always reflects reality.
    ///
    /// Events (§894) track the same reality, which is why they are not
    /// uniform across paths: a policy denial publishes `PolicyViolation`
    /// only, and a runtime-selection failure publishes none at all — neither
    /// task ever reached a runtime, and `TaskFailed` would contradict a state
    /// that is still `Queued`. `TaskStarted` is published once a runtime has
    /// been selected and the task is `Running`, matching that event's
    /// contract ("a task began executing on a runtime"); a consumer therefore
    /// gets `TaskStarted` if and only if the task really did start, and it
    /// repeats per attempt across the retry loop.
    pub fn execute(&self, task: &mut Task) -> Result<ExecutionResult> {
        // A fresh task enters the queue here; a task re-queued through the
        // §893 recovery loop (Failed → Rollback → Retry → Queued, see
        // `run_workflow`) is already Queued and re-enters directly.
        if task.state() == TaskState::Created {
            task.transition_to(TaskState::Queued)?;
        }

        if SENSITIVE_CAPABILITIES.contains(&task.capability) {
            if let Some((policy, identity)) = &self.access {
                let req = identity.request(format!("capability.{}", task.capability));
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
        // Published here, not on entry: `Event::TaskStarted` means "a task
        // began executing on a runtime", and only now is that true. Emitting
        // it first announced a runtime execution for tasks that a policy
        // denial or a missing runtime stopped before any runtime existed —
        // reproduced with an empty `RuntimeRegistry`, where `execute` returned
        // `Err`, the task ended `Queued`, and `TaskStarted` had already fired.
        self.bus.publish(Event::TaskStarted(task.id.clone()));

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
            // Nanoseconds: a local runtime serves a task in hundreds of them,
            // and `as_millis` recorded every such task as zero.
            latency_ns: started.elapsed().as_nanos().try_into().unwrap_or(u64::MAX),
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
                let mut task = task.clone();
                // §913: adopt the serving agent's declared scheduling priority
                // (`AgentManifest.priority`) when the task carries no explicit
                // (non-default) priority of its own, so an agent that declares
                // its work High/Critical actually dispatches sooner. An explicit
                // task priority (e.g. from a hand-authored workflow) wins.
                if task.priority == Priority::Normal {
                    if let Some(agent) = self.agents.discover(&task.capability).first() {
                        task.priority = agent.manifest.priority;
                    }
                }
                self.submit_scored_by_telemetry(&mut scheduler, task);
            }
        }

        let mut results = Vec::with_capacity(order.len());
        while let Some(mut task) = scheduler.dispatch_next() {
            match self.execute(&mut task) {
                Ok(result) => {
                    // Verification failure is retryable; once the budget is
                    // spent, propagate it instead of reporting the Failed
                    // result as if the workflow finished normally — mirrors
                    // the Err(e) arm below. Without this, a permanently
                    // failed task's id still reached `mark_completed`,
                    // incorrectly unblocking any dependent that requires it
                    // Completed, and WorkflowCompleted still fired for a
                    // workflow that never actually finished.
                    if result.state == TaskState::Failed {
                        if self.requeue(&mut scheduler, &mut task)? {
                            continue;
                        }
                        return Err(KernelError::other(format!(
                            "task {} ({}) exhausted its retry budget after failing verification",
                            result.task, result.capability
                        )));
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
    ///
    /// **No backoff, deliberately.** Production retry loops (AWS's
    /// exponential-backoff-and-jitter guidance, Kubernetes' CrashLoopBackOff,
    /// gRPC retry policies) delay each attempt so a failing dependency is not
    /// hammered. Two reasons this one does not, both checked rather than
    /// assumed:
    ///
    /// * It causes no starvation here. Measured with an always-failing task and
    ///   a healthy sibling in one workflow, the observed order was
    ///   `completed, bad-attempt, bad-attempt, bad-attempt` — the scheduler's
    ///   scoring dispatches ready work ahead of the doomed task's retries, so
    ///   retries do not block healthy tasks.
    /// * The obvious fix would make things worse. Sleeping inside `requeue`
    ///   would stall the single-threaded dispatch loop and create exactly the
    ///   head-of-line blocking the measurement shows is currently absent.
    ///
    /// If a genuinely remote runtime ever makes rapid retries harmful, the
    /// right shape is a *delayed requeue*: stamp the task with a "not before"
    /// dispatch cycle (the scheduler already keeps a monotonic clock for
    /// aging) and have `dispatch_next` skip it until then — spacing retries
    /// without blocking anyone else.
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
    ///
    /// The target stays in milliseconds for the caller; the comparison happens
    /// in microseconds, the unit the samples are recorded in.
    pub fn recommended_factors(&self, runtime: &str, target_latency_ms: u64) -> ScoreFactors {
        let target_ns = target_latency_ms.saturating_mul(1_000_000);
        let fit = match self.telemetry.mean_latency_ns_for(runtime) {
            Some(latency_ns) => runtime_fit(latency_ns.round() as u64, target_ns),
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
    use ckos_policy::AbacRule;
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
        assert!(engine.telemetry().mean_latency_ns_for("echo").is_some());

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
    fn telemetry_measures_sub_millisecond_work_instead_of_recording_zero() {
        // A local runtime finishes in microseconds. Recording latency in whole
        // milliseconds truncated every such task to 0, so telemetry reported a
        // *measured* zero latency and zero throughput for work that did run.
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Reasoning])));
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new());
        let mut task = Task::new("answer the scheduling question", Capability::Reasoning);
        engine.execute(&mut task).expect("task runs");

        let tel = engine.telemetry();
        let mean = tel.mean_latency_ms().expect("a sample was recorded");
        assert!(mean > 0.0, "sub-millisecond work recorded as {mean}ms");
        let rate = tel
            .mean_tokens_per_sec()
            .expect("measurable elapsed time yields a rate");
        assert!(rate > 0.0, "throughput reported as {rate} tok/s");

        // The scheduler loop sees the real figure rather than the "unknown"
        // branch every sub-millisecond runtime used to fall into.
        let observed = tel
            .mean_latency_ns_for("echo")
            .expect("the echo runtime has samples");
        assert!(observed > 0.0, "runtime latency recorded as {observed} ns");
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
    fn task_started_is_published_only_when_a_runtime_really_started_the_task() {
        // Regression: `Event::TaskStarted` is documented as "a task began
        // executing on a runtime", but it was published as `execute`'s very
        // first statement — before the policy gate and before any runtime was
        // selected. So a task stopped by either gate announced a runtime
        // execution that never happened, contradicting the module's own
        // invariant that the observable state reflects reality. Reproduced
        // with an empty `RuntimeRegistry`: `execute` returned `Err`, the task
        // ended `Queued`, and `TaskStarted` had already fired once.
        //
        // Both never-started paths are covered, plus the positive case — a
        // fix that simply stopped publishing the event would pass a
        // negative-only test.
        let count_starts = |engine: &Engine| {
            let n = Arc::new(AtomicUsize::new(0));
            let seen = Arc::clone(&n);
            engine.bus().subscribe(Arc::new(move |e: &Event| {
                if matches!(e, Event::TaskStarted(_)) {
                    seen.fetch_add(1, Ordering::SeqCst);
                }
            }));
            n
        };

        // 1. No runtime can serve the capability.
        let engine = Engine::new(
            RuntimeRegistry::new(),
            CapabilityRegistry::new(),
            Verifier::new(),
        );
        let starts = count_starts(&engine);
        let mut task = Task::new("nothing can serve this", Capability::Reasoning);
        assert!(engine.execute(&mut task).is_err());
        assert_eq!(task.state(), TaskState::Queued, "it never started");
        assert_eq!(
            starts.load(Ordering::SeqCst),
            0,
            "a task with no runtime must not announce that it started on one"
        );

        // 2. Denied by policy, before a runtime is ever selected.
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Medical])));
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new())
            .with_policy(PolicyEngine::new(), vec!["guest".to_string()]);
        let starts = count_starts(&engine);
        let mut task = Task::new("diagnose", Capability::Medical);
        assert!(engine.execute(&mut task).is_err());
        assert_eq!(task.state(), TaskState::Queued);
        assert_eq!(
            starts.load(Ordering::SeqCst),
            0,
            "a policy-denied task must not announce that it started"
        );

        // 3. A task that genuinely runs still publishes exactly one.
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Reasoning])));
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new());
        let starts = count_starts(&engine);
        let mut task = Task::new("summarize", Capability::Reasoning);
        assert!(engine.execute(&mut task).is_ok());
        assert_eq!(task.state(), TaskState::Completed);
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "a task that did start must still announce it"
        );
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

    #[test]
    fn abac_attributes_from_a_real_identity_change_the_authorization_outcome() {
        // Proves attributes actually flow Identity -> AccessRequest -> ABAC,
        // not just RBAC roles: an admin's blanket RBAC grant is overridden by
        // an explicit ABAC deny keyed on an attribute the identity carries.
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Medical])));

        let mut policy = PolicyEngine::new();
        policy.grant("admin", "capability.*");
        policy.add_rule(AbacRule {
            action: "capability.medical".into(),
            attribute_key: "region".into(),
            attribute_value: "restricted".into(),
            deny: true,
        });

        // Same role, restricted region -> ABAC deny wins over the RBAC grant.
        let restricted = Identity::new("alice")
            .with_role("admin")
            .with_attribute("region", "restricted");
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new())
            .with_identity(policy, restricted);
        let mut task = Task::new("diagnose", Capability::Medical);
        assert!(engine.execute(&mut task).is_err());
        assert_eq!(task.state(), TaskState::Queued);

        // Same role, unrestricted region -> the RBAC grant applies normally.
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Medical])));
        let mut policy = PolicyEngine::new();
        policy.grant("admin", "capability.*");
        policy.add_rule(AbacRule {
            action: "capability.medical".into(),
            attribute_key: "region".into(),
            attribute_value: "restricted".into(),
            deny: true,
        });
        let unrestricted = Identity::new("alice")
            .with_role("admin")
            .with_attribute("region", "hq");
        let engine = Engine::new(runtimes, CapabilityRegistry::new(), Verifier::new())
            .with_identity(policy, unrestricted);
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

    /// A check that always fails, so a task's output can never pass
    /// verification no matter how many times it's retried — the
    /// verification-failure counterpart to `FlakyRuntime`'s runtime-error
    /// exhaustion above.
    struct AlwaysFailCheck;
    impl ckos_verifier::Check for AlwaysFailCheck {
        fn name(&self) -> &str {
            "always_fail"
        }
        fn evaluate(&self, _output: &str) -> ckos_verifier::Verdict {
            ckos_verifier::Verdict::Fail("never passes".into())
        }
    }

    #[test]
    fn exhausted_verification_failure_is_propagated_not_reported_as_completed() {
        // A runtime that always succeeds, paired with a verifier that never
        // passes: retries exhaust via the Ok(result)/verification-failure
        // arm, not the Err(e)/runtime-error arm `deterministic_failure_...`
        // above already covers. Before the fix, this arm silently fell
        // through to `mark_completed` + `results.push`, indistinguishable
        // from genuine success.
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Reasoning])));
        let engine = Engine::new(
            runtimes,
            CapabilityRegistry::new(),
            Verifier::new().with_check(Box::new(AlwaysFailCheck)),
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
        assert!(err.to_string().contains("exhausted its retry budget"));
        // A workflow that never actually finished must never publish
        // WorkflowCompleted.
        assert_eq!(completed.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_task_that_exhausts_verification_never_unblocks_its_dependents() {
        // Two-step chain b <- a, both served by an always-succeeding runtime
        // but an always-failing verifier: `a` exhausts its retry budget, and
        // `b` (which depends on `a` reaching a genuine Completed state) must
        // never dispatch. Before the fix, `mark_completed` ran for `a`
        // regardless of its real Failed state, so `b` ran anyway.
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![
            Capability::Reasoning,
            Capability::Coding,
        ])));
        let engine = Engine::new(
            runtimes,
            CapabilityRegistry::new(),
            Verifier::new().with_check(Box::new(AlwaysFailCheck)),
        );

        let mut dag = Dag::new("chain");
        let a = dag.add_step(Task::new("a", Capability::Reasoning), &[]);
        dag.add_step(Task::new("b", Capability::Coding), &[a]);

        let err = engine.run_workflow(&dag).unwrap_err();
        assert!(err.to_string().contains("exhausted its retry budget"));
        // `b` must never have been dispatched: `run_workflow` returns before
        // the dispatch loop can reach it, so the audit log — one record per
        // execute() attempt — must contain only `a`'s exhausted attempts
        // (1 initial + MAX_TASK_RETRIES retries), never one for `b`.
        assert_eq!(
            engine.audit().snapshot().len(),
            1 + MAX_TASK_RETRIES as usize,
            "only `a`'s attempts should be audited; `b` must never dispatch"
        );
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
        for (rt, latency_ms) in [("slow-rt", 500), ("fast-rt", 1)] {
            engine.telemetry().record(TaskMetrics {
                runtime: rt.into(),
                latency_ns: latency_ms * 1_000_000,
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
    fn agent_declared_priority_orders_dispatch() {
        // §913: AgentManifest.priority was parsed but never consumed. Two
        // independent, equal-runtime tasks; the one whose serving agent
        // declares Critical priority must dispatch first, even though its step
        // is added second (so a regression to first-registered tie-breaking
        // would pick the other one).
        let mut runtimes = RuntimeRegistry::new();
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Vision])));
        runtimes.register(Box::new(EchoRuntime::new(vec![Capability::Retrieval])));

        let mut agents = CapabilityRegistry::new();
        agents.register(AgentManifest::new("vision", Capability::Vision)); // Normal
        let mut important = AgentManifest::new("retrieval", Capability::Retrieval);
        important.priority = Priority::Critical;
        agents.register(important);

        let engine = Engine::new(runtimes, agents, Verifier::new());

        let mut dag = Dag::new("mixed");
        dag.add_step(Task::new("look", Capability::Vision), &[]); // added first
        dag.add_step(Task::new("fetch", Capability::Retrieval), &[]);
        let results = engine.run_workflow(&dag).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].capability,
            Capability::Retrieval,
            "the Critical-priority agent's task must dispatch first"
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
