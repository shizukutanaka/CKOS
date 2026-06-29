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

use ckos_kernel::capability::Capability;
use ckos_kernel::error::{KernelError, Result};
use ckos_kernel::event::{Event, EventBus, InMemoryEventBus};
use ckos_kernel::task::Task;
use ckos_kernel::TaskId;
use ckos_runtime::{InferenceRequest, RuntimeRegistry};
use ckos_scheduler::Scheduler;
use ckos_verifier::Verifier;
use ckos_workflow::Dag;

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
}

impl Engine {
    /// Build an engine from its subsystems.
    pub fn new(runtimes: RuntimeRegistry, agents: CapabilityRegistry, verifier: Verifier) -> Self {
        Engine {
            runtimes,
            agents,
            verifier,
            bus: InMemoryEventBus::new(),
        }
    }

    /// The event bus, so callers can subscribe to execution events (§894).
    pub fn bus(&self) -> &InMemoryEventBus {
        &self.bus
    }

    /// Execute one task: select runtime → run → verify, emitting events.
    pub fn execute(&self, task: &Task) -> Result<ExecutionResult> {
        self.bus.publish(Event::TaskStarted(task.id.clone()));

        let agent = self
            .agents
            .discover(&task.capability)
            .first()
            .map(|a| a.manifest.id.clone());

        let runtime = self.runtimes.select(&task.capability)?;
        let runtime_name = runtime.name().to_string();
        let response = runtime.run(&InferenceRequest {
            input: task.description.clone(),
            capability: task.capability.clone(),
            max_tokens: 512,
        })?;

        let report = self.verifier.verify(&response.output);
        let verified = report.passed();
        if verified {
            self.bus.publish(Event::TaskCompleted(task.id.clone()));
        } else {
            let reason = report
                .failures()
                .iter()
                .map(|(n, w)| format!("{n}: {w}"))
                .collect::<Vec<_>>()
                .join("; ");
            self.bus.publish(Event::TaskFailed {
                task: task.id.clone(),
                reason,
            });
        }

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
    /// [`KernelError::Other`] if the DAG contains a cycle, or propagates a task
    /// failure (e.g. no runtime for a capability).
    pub fn run_workflow(&self, dag: &Dag) -> Result<Vec<ExecutionResult>> {
        let order = dag
            .topological_order()
            .ok_or_else(|| KernelError::other("workflow contains a cycle"))?;

        let mut scheduler = Scheduler::new();
        for step in &order {
            if let Some(task) = dag.task(*step) {
                scheduler.submit(task.clone());
            }
        }
        self.bus.publish(Event::WorkflowCompleted(dag.id().clone()));

        let mut results = Vec::with_capacity(order.len());
        while let Some(task) = scheduler.dispatch_next() {
            let result = self.execute(&task)?;
            scheduler.mark_completed(task.id.clone());
            results.push(result);
        }
        Ok(results)
    }

    /// Self-evaluate a batch of results with the given reflector (§921).
    pub fn reflect(&self, reflector: &dyn Reflector, results: &[ExecutionResult]) -> Vec<Reflection> {
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
    }

    #[test]
    fn missing_runtime_fails_the_task() {
        // Engine with no runtime registered for the required capability.
        let engine = Engine::new(
            RuntimeRegistry::new(),
            CapabilityRegistry::new(),
            Verifier::new(),
        );
        let dag = HeuristicPlanner::new().plan("say hello");
        let err = engine.run_workflow(&dag).unwrap_err();
        assert!(matches!(err, KernelError::CapabilityUnavailable(_)));
    }
}
