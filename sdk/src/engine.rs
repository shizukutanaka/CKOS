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
use ckos_kernel::task::Task;
use ckos_kernel::telemetry::{InMemoryTelemetry, TaskMetrics, TelemetrySink};
use ckos_kernel::TaskId;
use ckos_runtime::{InferenceRequest, RuntimeRegistry};
use ckos_scheduler::{runtime_fit, Scheduler, ScoreFactors};
use ckos_verifier::Verifier;
use ckos_workflow::Dag;
use std::time::Instant;

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
}

/// Orchestrates a workflow across runtimes, agents and the verifier.
pub struct Engine {
    runtimes: RuntimeRegistry,
    agents: CapabilityRegistry,
    verifier: Verifier,
    bus: InMemoryEventBus,
    audit: InMemoryAuditLog,
    telemetry: InMemoryTelemetry,
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
        }
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
    /// writing an audit record (§903) on every path, success or failure.
    pub fn execute(&self, task: &Task) -> Result<ExecutionResult> {
        self.bus.publish(Event::TaskStarted(task.id.clone()));

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

        let started = Instant::now();
        let response = match runtime.run(&InferenceRequest {
            input: task.description.clone(),
            capability: task.capability.clone(),
            max_tokens: 512,
        }) {
            Ok(r) => r,
            Err(e) => {
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

        let report = self.verifier.verify(&response.output);
        let verified = report.passed();
        let mut record = AuditRecord::new("task.execute")
            .runtime(&runtime_name)
            .input(&task.description)
            .output(&response.output);
        if verified {
            self.bus.publish(Event::TaskCompleted(task.id.clone()));
        } else {
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
        })
    }

    /// Run a whole workflow in dependency order via the scheduler (§892).
    ///
    /// Returns results in execution order. Fails fast with
    /// [`KernelError::Other`] if the DAG cannot be ordered (a cycle, or a
    /// dangling/foreign step reference — see [`Dag::topological_order`]), or
    /// propagates a task failure (e.g. no runtime for a capability).
    pub fn run_workflow(&self, dag: &Dag) -> Result<Vec<ExecutionResult>> {
        let order = dag.topological_order().ok_or_else(|| {
            KernelError::other("workflow contains a cycle or references an unknown step")
        })?;

        let mut scheduler = Scheduler::new();
        for step in &order {
            if let Some(task) = dag.task(*step) {
                scheduler.submit(task.clone());
            }
        }

        let mut results = Vec::with_capacity(order.len());
        while let Some(task) = scheduler.dispatch_next() {
            // Propagate a task failure immediately, without publishing
            // WorkflowCompleted — a subscriber must never observe that event
            // for a workflow that didn't actually finish.
            let result = self.execute(&task)?;
            scheduler.mark_completed(task.id.clone());
            results.push(result);
        }
        self.bus.publish(Event::WorkflowCompleted(dag.id().clone()));
        Ok(results)
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
