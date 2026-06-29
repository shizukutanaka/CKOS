//! # CKOS Workflow Engine
//!
//! Workflows are directed acyclic graphs (§895). Each step wraps a kernel
//! [`Task`]; edges encode dependencies. Independent steps can run in parallel,
//! and [`Dag::topological_order`] yields a legal execution order (or `None` if
//! a cycle was introduced — the "acyclic" invariant is checked, not assumed).

use ckos_kernel::task::Task;
use ckos_kernel::WorkflowId;

/// A handle to a step within a [`Dag`]. Cheap to copy and pass around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepRef(usize);

struct Step {
    task: Task,
    deps: Vec<usize>,
}

/// A workflow DAG (§895).
pub struct Dag {
    id: WorkflowId,
    name: String,
    steps: Vec<Step>,
}

impl Dag {
    /// Create an empty DAG with a descriptive name.
    pub fn new(name: impl Into<String>) -> Self {
        Dag {
            id: WorkflowId::new(),
            name: name.into(),
            steps: Vec::new(),
        }
    }

    /// Workflow id.
    pub fn id(&self) -> &WorkflowId {
        &self.id
    }

    /// Workflow name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Add a step depending on zero or more existing steps. Returns its handle.
    pub fn add_step(&mut self, mut task: Task, deps: &[StepRef]) -> StepRef {
        task.workflow = Some(self.id.clone());
        let idx = self.steps.len();
        self.steps.push(Step {
            task,
            deps: deps.iter().map(|r| r.0).collect(),
        });
        StepRef(idx)
    }

    /// Number of steps.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the DAG has no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Borrow the task behind a step handle.
    pub fn task(&self, step: StepRef) -> Option<&Task> {
        self.steps.get(step.0).map(|s| &s.task)
    }

    /// Steps with no dependencies — the initial parallel frontier.
    pub fn roots(&self) -> Vec<StepRef> {
        self.steps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.deps.is_empty())
            .map(|(i, _)| StepRef(i))
            .collect()
    }

    /// Kahn's algorithm: a valid execution order, or `None` if a cycle exists.
    pub fn topological_order(&self) -> Option<Vec<StepRef>> {
        let n = self.steps.len();
        let mut indegree = vec![0usize; n];
        // Build dependents map: dep -> [steps depending on it].
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, s) in self.steps.iter().enumerate() {
            for &d in &s.deps {
                if d >= n {
                    return None; // dangling dependency
                }
                indegree[i] += 1;
                dependents[d].push(i);
            }
        }
        let mut ready: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(node) = ready.pop() {
            order.push(StepRef(node));
            for &next in &dependents[node] {
                indegree[next] -= 1;
                if indegree[next] == 0 {
                    ready.push(next);
                }
            }
        }
        if order.len() == n {
            Some(order)
        } else {
            None // a cycle prevented full ordering
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckos_kernel::Capability;

    #[test]
    fn linear_pipeline_orders_correctly() {
        let mut dag = Dag::new("pipeline");
        let a = dag.add_step(Task::new("a", Capability::Retrieval), &[]);
        let b = dag.add_step(Task::new("b", Capability::Reasoning), &[a]);
        let _c = dag.add_step(Task::new("c", Capability::Verification), &[b]);
        let order = dag.topological_order().unwrap();
        assert_eq!(order.len(), 3);
        assert_eq!(order[0], a); // root first
        assert_eq!(dag.roots(), vec![a]);
    }

    #[test]
    fn parallel_steps_share_a_root_frontier() {
        let mut dag = Dag::new("fan-out");
        let root = dag.add_step(Task::new("root", Capability::Planning), &[]);
        dag.add_step(Task::new("left", Capability::Coding), &[root]);
        dag.add_step(Task::new("right", Capability::Coding), &[root]);
        assert_eq!(dag.topological_order().unwrap().len(), 3);
    }
}
