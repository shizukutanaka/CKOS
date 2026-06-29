//! # CKOS Workflow Engine
//!
//! Workflows are directed acyclic graphs (§895). Each step wraps a kernel
//! [`Task`]; edges encode dependencies. Independent steps can run in parallel,
//! and [`Dag::topological_order`] yields a legal execution order (or `None` if
//! a cycle was introduced — the "acyclic" invariant is checked, not assumed).

use ckos_kernel::capability::Capability;
use ckos_kernel::error::{KernelError, Result};
use ckos_kernel::task::Task;
use ckos_kernel::WorkflowId;
use std::collections::HashMap;

/// A handle to a step within a [`Dag`]. Cheap to copy and pass around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepRef(usize);

struct Step {
    task: Task,
    deps: Vec<usize>,
}

/// Escape a string for use inside a Graphviz double-quoted label.
pub(crate) fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
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
    ///
    /// The dependency steps' task ids are copied into the new task's
    /// [`Task::depends_on`] so the task is self-describing and can be handed
    /// straight to the scheduler, whose dependency resolver gates on task ids.
    pub fn add_step(&mut self, mut task: Task, deps: &[StepRef]) -> StepRef {
        task.workflow = Some(self.id.clone());
        for r in deps {
            if let Some(dep_step) = self.steps.get(r.0) {
                task.depends_on.push(dep_step.task.id.clone());
            }
        }
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

    /// Load a DAG from a declarative definition (§920 workflow compiler):
    ///
    /// ```text
    /// workflow: research pipeline
    /// step search: retrieval
    /// step embed: embedding <- search
    /// step summarize: reasoning <- embed
    /// step report: reasoning <- summarize
    /// ```
    ///
    /// Each `step <name>: <capability> [<- dep, …]` line adds a node; deps name
    /// earlier steps. Requiring deps to be already-defined keeps the graph
    /// acyclic by construction. Dependency-free (no YAML/TOML crate).
    pub fn from_definition(text: &str) -> Result<Dag> {
        let mut name = "workflow".to_string();
        let mut dag: Option<Dag> = None;
        let mut refs: HashMap<String, StepRef> = HashMap::new();

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("workflow:") {
                name = rest.trim().to_string();
                continue;
            }
            let Some(rest) = line.strip_prefix("step ") else {
                return Err(KernelError::other(format!("unexpected line: {line}")));
            };
            let (step_name, spec) = rest
                .split_once(':')
                .ok_or_else(|| KernelError::other(format!("step missing ':' — {line}")))?;
            let step_name = step_name.trim();
            if step_name.is_empty() {
                return Err(KernelError::other("step missing a name"));
            }
            let (cap_part, dep_part) = match spec.split_once("<-") {
                Some((c, d)) => (c.trim(), Some(d.trim())),
                None => (spec.trim(), None),
            };
            if cap_part.is_empty() {
                return Err(KernelError::other(format!(
                    "step {step_name} missing capability"
                )));
            }
            let capability: Capability = cap_part
                .parse()
                .unwrap_or(Capability::Custom(cap_part.into()));

            let mut deps = Vec::new();
            if let Some(d) = dep_part {
                for dep in d.split(',') {
                    let dep = dep.trim();
                    if dep.is_empty() {
                        continue;
                    }
                    let r = refs.get(dep).copied().ok_or_else(|| {
                        KernelError::other(format!(
                            "step {step_name} depends on unknown step {dep}"
                        ))
                    })?;
                    deps.push(r);
                }
            }

            let dag = dag.get_or_insert_with(|| Dag::new(&name));
            let r = dag.add_step(Task::new(step_name, capability), &deps);
            refs.insert(step_name.to_string(), r);
        }

        dag.ok_or_else(|| KernelError::other("workflow has no steps"))
    }

    /// Render the DAG as a Graphviz DOT digraph for visualization (a building
    /// block for the v2.8 Workflow Studio). Nodes show the step description and
    /// capability; edges are dependencies.
    pub fn to_dot(&self) -> String {
        let mut s = String::from("digraph workflow {\n  rankdir=LR;\n");
        for (i, step) in self.steps.iter().enumerate() {
            s.push_str(&format!(
                "  n{i} [label=\"{}\\n[{}]\"];\n",
                dot_escape(&step.task.description),
                step.task.capability
            ));
        }
        for (i, step) in self.steps.iter().enumerate() {
            for d in &step.deps {
                s.push_str(&format!("  n{d} -> n{i};\n"));
            }
        }
        s.push_str("}\n");
        s
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

        // Dependency edges are copied into the task so the scheduler can gate on
        // them: step `b` depends on step `a`'s task id.
        let a_id = dag.task(a).unwrap().id.clone();
        assert_eq!(dag.task(b).unwrap().depends_on, vec![a_id]);
    }

    #[test]
    fn loads_from_definition() {
        let text = "\
workflow: research pipeline
step search: retrieval
step embed: embedding <- search
step summarize: reasoning <- embed
step report: reasoning <- summarize, search";
        let dag = Dag::from_definition(text).unwrap();
        assert_eq!(dag.name(), "research pipeline");
        assert_eq!(dag.len(), 4);
        let order = dag.topological_order().unwrap();
        assert_eq!(order.len(), 4);
        // search has no deps → it is a root.
        assert_eq!(dag.roots().len(), 1);
    }

    #[test]
    fn rejects_unknown_dependency() {
        // Dag isn't Debug, so avoid unwrap_err and match explicitly.
        match Dag::from_definition("step a: coding <- ghost") {
            Err(e) => assert!(e.to_string().contains("unknown step")),
            Ok(_) => panic!("expected an error for the unknown dependency"),
        }
    }

    #[test]
    fn exports_graphviz_dot() {
        let mut dag = Dag::new("pipeline");
        let a = dag.add_step(Task::new("a", Capability::Retrieval), &[]);
        dag.add_step(Task::new("b", Capability::Reasoning), &[a]);
        let dot = dag.to_dot();
        assert!(dot.starts_with("digraph workflow {"));
        assert!(dot.contains("n0 -> n1;")); // dependency edge
        assert!(dot.contains("[retrieval]"));
        assert!(dot.trim_end().ends_with('}'));
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
