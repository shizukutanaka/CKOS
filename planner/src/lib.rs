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
            let deps: Vec<StepRef> = st
                .depends_on
                .iter()
                .filter_map(|i| refs.get(*i).copied())
                .collect();
            let r = dag.add_step(task, &deps);
            refs.push(r);
        }
        dag
    }
}

/// A dependency-free heuristic planner that classifies an intent (research,
/// coding, translation, question, or generic) and emits the matching capability
/// pipeline — the research case being the canonical §895 flow. A model-backed
/// planner can replace it behind the [`Planner`] trait.
///
/// **Deliberately does not classify regulated domains** (finance/medical/
/// legal/robotics — see `ckos_sdk::engine::SENSITIVE_CAPABILITIES`). A
/// keyword list for those domains was prototyped and rejected: tested against
/// realistic paraphrases of a medical question, it missed 3 of 4 (only an
/// exact "diagnose"/"symptom"/... hit was ever caught). A safety gate with
/// that miss rate is worse than no gate — it invites operators to trust
/// `ckos run --role` to catch sensitive content when it demonstrably can't,
/// which is also the same lexical-only limitation already documented for
/// `HashingEmbedder`. Detecting regulated-domain intent from free text is
/// itself a form of judgment §891 keeps out of this kernel; the honest
/// contract is that only an explicit capability declaration (e.g. a
/// hand-authored `step x: medical` in a `ckos workflow` file, or a
/// custom `Planner`/agent) reaches those capabilities — never this planner's
/// heuristics. See `heuristic_planner_never_infers_regulated_capabilities`
/// below, which pins this down as regression-tested behaviour, not
/// unexamined absence.
#[derive(Default)]
pub struct HeuristicPlanner;

impl HeuristicPlanner {
    /// Create the planner.
    pub fn new() -> Self {
        Self
    }
}

/// Intent categories the heuristic planner recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    Research,
    Coding,
    Translation,
    Question,
    Generic,
}

fn classify(intent: &str) -> Intent {
    let q = intent.to_lowercase();
    let any = |keys: &[&str]| keys.iter().any(|k| q.contains(k));
    if any(&["research", "paper", "論文", "report", "レポート"]) {
        Intent::Research
    } else if any(&[
        "code",
        "implement",
        "function",
        "bug",
        "refactor",
        "コード",
        "関数",
        "実装",
    ]) {
        Intent::Coding
    } else if any(&["translate", "translation", "翻訳"]) {
        Intent::Translation
    } else if intent.trim_end().ends_with('?')
        || any(&[
            "what", "why", "how", "explain", "なに", "なぜ", "どう", "説明",
        ])
    {
        Intent::Question
    } else {
        Intent::Generic
    }
}

impl Planner for HeuristicPlanner {
    fn decompose(&self, intent: &str) -> Vec<SubTask> {
        let step = |desc: &str, cap: Capability, deps: Vec<usize>| SubTask {
            description: desc.into(),
            capability: cap,
            depends_on: deps,
        };
        match classify(intent) {
            // §895: search -> embed -> summarize -> verify citations -> report
            Intent::Research => vec![
                step("search sources", Capability::Retrieval, vec![]),
                step("generate embeddings", Capability::Embedding, vec![0]),
                step("summarize", Capability::Reasoning, vec![1]),
                step("verify citations", Capability::Verification, vec![2]),
                step("generate report", Capability::Reasoning, vec![3]),
            ],
            // plan -> write code -> verify (static analysis, §899)
            Intent::Coding => vec![
                step("plan implementation", Capability::Planning, vec![]),
                step("write code", Capability::Coding, vec![0]),
                step("verify code", Capability::Verification, vec![1]),
            ],
            // single translation step
            Intent::Translation => vec![step("translate", Capability::Translation, vec![])],
            // retrieve context -> answer
            Intent::Question => vec![
                step("retrieve context", Capability::Retrieval, vec![]),
                step("answer", Capability::Reasoning, vec![0]),
            ],
            Intent::Generic => vec![step(intent, Capability::Reasoning, vec![])],
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

    #[test]
    fn coding_intent_plans_code_and_verify() {
        let dag = HeuristicPlanner::new().plan("implement a function to sort a list");
        assert_eq!(dag.len(), 3);
        let order = dag.topological_order().unwrap();
        assert_eq!(dag.task(order[0]).unwrap().capability, Capability::Planning);
        assert!(dag
            .topological_order()
            .unwrap()
            .iter()
            .any(|s| dag.task(*s).unwrap().capability == Capability::Coding));
    }

    #[test]
    fn question_intent_retrieves_then_answers() {
        let dag = HeuristicPlanner::new().plan("why is the sky blue?");
        assert_eq!(dag.len(), 2);
        let order = dag.topological_order().unwrap();
        assert_eq!(
            dag.task(order[0]).unwrap().capability,
            Capability::Retrieval
        );
    }

    #[test]
    fn heuristic_planner_never_infers_regulated_capabilities() {
        // Locks in the module doc's claim: no input, however clearly about a
        // regulated domain to a human reader, ever produces
        // Finance/Medical/Legal/Robotics from this planner — including
        // prompts an exact-keyword classifier would miss (the reason that
        // approach was rejected; see the module doc). If this planner is ever
        // intentionally extended to classify these domains, this test must
        // fail and be updated deliberately, not silently.
        let prompts = [
            "diagnose patient symptoms",
            "what pill helps a headache",
            "should I take ibuprofen with my blood thinner",
            "is this mole cancerous",
            "should I sue my landlord",
            "is this contract legally binding",
            "how much tax do I owe on this investment",
            "move the robot arm to pick up the part",
        ];
        let regulated = [
            Capability::Finance,
            Capability::Medical,
            Capability::Legal,
            Capability::Robotics,
        ];
        for p in prompts {
            let dag = HeuristicPlanner::new().plan(p);
            for step in dag.topological_order().unwrap() {
                let cap = &dag.task(step).unwrap().capability;
                assert!(
                    !regulated.contains(cap),
                    "planner must never infer a regulated capability; \
                     {p:?} produced {cap} — see this planner's module doc"
                );
            }
        }
    }

    #[test]
    fn translation_intent_is_single_translation_step() {
        let dag = HeuristicPlanner::new().plan("translate this to French");
        assert_eq!(dag.len(), 1);
        let order = dag.topological_order().unwrap();
        assert_eq!(
            dag.task(order[0]).unwrap().capability,
            Capability::Translation
        );
    }
}
