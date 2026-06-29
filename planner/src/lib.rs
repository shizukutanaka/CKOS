//! # CKOS Planner
//!
//! Converts an intent into an execution plan (§898, §920):
//!
//! ```text
//! input -> intent analysis -> subtask decomposition -> dependency analysis
//!       -> DAG generation -> hand off to the scheduler
//! ```
//!
//! The kernel performs no inference (§891), so the planner is structured as a
//! pipeline of pluggable stages. The default [`HeuristicPlanner`] decomposes
//! intent without a model — enough to build and exercise the DAG machinery —
//! while a model-backed planner can implement the same [`Planner`] trait.

use ckos_kernel::capability::Capability;
use ckos_kernel::task::Task;
use ckos_workflow::{Dag, StepRef};

/// A planned, decomposed subtask before it becomes a DAG node.
#[derive(Debug, Clone)]
pub struct SubTask {
    /// What the step should do.
    pub description: String,
    /// Capability required to execute it.
    pub capability: Capability,
    /// Indices (into the plan's subtask list) this step depends on.
    pub depends_on: Vec<usize>,
}

/// Trait implemented by planning strategies (§898).
pub trait Planner {
    /// Decompose an intent into ordered subtasks.
    fn decompose(&self, intent: &str) -> Vec<SubTask>;

    /// Build a workflow DAG (§895) from an intent, wiring dependencies.
    fn plan(&self, intent: &str) -> Dag {
        let subtasks = self.decompose(intent);
        let mut dag = Dag::new(intent);
        let mut refs: Vec<StepRef> = Vec::with_capacity(subtasks.len());
        for st in &subtasks {
            let task = Task::new(st.description.clone(), st.capability.clone());
            let deps: Vec<StepRef> = st.depends_on.iter().filter_map(|i| refs.get(*i).copied()).collect();
            let r = dag.add_step(task, &deps);
            refs.push(r);
        }
        dag
    }
}

/// A dependency-free heuristic planner producing the canonical research
/// pipeline from §895 when it recognises a "research"-shaped intent, and a
/// generic single-step plan otherwise.
#[derive(Default)]
pub struct HeuristicPlanner;

impl HeuristicPlanner {
    /// Create the planner.
    pub fn new() -> Self {
        Self
    }
}

impl Planner for HeuristicPlanner {
    fn decompose(&self, intent: &str) -> Vec<SubTask> {
        let lower = intent.to_lowercase();
        let looks_like_research = ["research", "paper", "論文", "report", "レポート"]
            .iter()
            .any(|k| lower.contains(k));

        if looks_like_research {
            // §895: search -> embed -> summarize -> verify citations -> report
            vec![
                SubTask {
                    description: "search sources".into(),
                    capability: Capability::Retrieval,
                    depends_on: vec![],
                },
                SubTask {
                    description: "generate embeddings".into(),
                    capability: Capability::Embedding,
                    depends_on: vec![0],
                },
                SubTask {
                    description: "summarize".into(),
                    capability: Capability::Reasoning,
                    depends_on: vec![1],
                },
                SubTask {
                    description: "verify citations".into(),
                    capability: Capability::Verification,
                    depends_on: vec![2],
                },
                SubTask {
                    description: "generate report".into(),
                    capability: Capability::Reasoning,
                    depends_on: vec![3],
                },
            ]
        } else {
            vec![SubTask {
                description: intent.to_string(),
                capability: Capability::Reasoning,
                depends_on: vec![],
            }]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn research_intent_builds_five_step_pipeline() {
        let dag = HeuristicPlanner::new().plan("research the Transformer paper");
        assert_eq!(dag.len(), 5);
        // The DAG must be schedulable (acyclic, topologically orderable).
        assert!(dag.topological_order().is_some());
    }

    #[test]
    fn generic_intent_is_single_step() {
        let dag = HeuristicPlanner::new().plan("say hello");
        assert_eq!(dag.len(), 1);
    }
}
