//! Reflection — self-evaluation after execution (§921) and consensus across
//! agents (§922).
//!
//! After a task runs, a [`Reflector`] scores the [`ExecutionResult`] and emits
//! an improvement hint; the hint can be persisted to memory (§921 "→ memory")
//! so future planning can learn from it (the §959 learning loop). When several
//! agents reflect on the same work, [`consensus`] merges their verdicts (§922)
//! — surfacing improvements a single agent would miss.

use crate::engine::ExecutionResult;
use ckos_kernel::error::Result;
use ckos_kernel::{DocumentId, TaskId};
use ckos_memory::{Document, Storage};

/// A single self-evaluation of a task result (§921).
#[derive(Debug, Clone)]
pub struct Reflection {
    /// Task that was evaluated.
    pub task: TaskId,
    /// Quality score, 0..=100 (§948 confidence scale).
    pub score: u8,
    /// Actionable improvement hint.
    pub hint: String,
}

/// Strategy that scores a result and proposes an improvement (§921).
pub trait Reflector: Send + Sync {
    /// Evaluate one execution result.
    fn reflect(&self, result: &ExecutionResult) -> Reflection;
}

/// A model-free reflector that scores on verification, output presence and
/// whether a specialized agent was available — enough to drive the loop without
/// inference, replaceable by a model-backed reflector behind the same trait.
#[derive(Default)]
pub struct HeuristicReflector;

impl HeuristicReflector {
    /// Create the reflector.
    pub fn new() -> Self {
        Self
    }
}

impl Reflector for HeuristicReflector {
    fn reflect(&self, result: &ExecutionResult) -> Reflection {
        let (score, hint) = if !result.verified {
            (
                20,
                "output failed verification; regenerate with stricter constraints",
            )
        } else if result.output.trim().is_empty() {
            (40, "verified but empty; tighten the prompt to require content")
        } else if result.agent.is_none() {
            (
                70,
                "no specialized agent matched; register one for this capability",
            )
        } else {
            (95, "result accepted; cache for reuse")
        };
        Reflection {
            task: result.task.clone(),
            score,
            hint: hint.to_string(),
        }
    }
}

/// Aggregated verdict over multiple reflections (§922).
#[derive(Debug, Clone)]
pub struct Consensus {
    /// Mean score across reflections.
    pub score: u8,
    /// Distinct hints, in first-seen order.
    pub hints: Vec<String>,
}

/// Combine several reflections into a consensus (§922). Empty input yields a
/// zero-score, hint-less consensus.
pub fn consensus(reflections: &[Reflection]) -> Consensus {
    if reflections.is_empty() {
        return Consensus {
            score: 0,
            hints: Vec::new(),
        };
    }
    let sum: u32 = reflections.iter().map(|r| r.score as u32).sum();
    let score = (sum / reflections.len() as u32) as u8;
    let mut hints = Vec::new();
    for r in reflections {
        if !hints.contains(&r.hint) {
            hints.push(r.hint.clone());
        }
    }
    Consensus { score, hints }
}

/// Persist a reflection to memory as a `reflection` document (§921), tagging it
/// with the originating task and carrying the score as the document confidence.
pub fn store_reflection(store: &mut dyn Storage, reflection: &Reflection) -> Result<DocumentId> {
    let mut doc = Document::new(
        "reflection",
        format!("reflection for {}", reflection.task),
        reflection.hint.clone(),
    );
    doc.confidence = reflection.score;
    doc.metadata
        .insert("task".to_string(), reflection.task.to_string());
    let id = doc.id.clone();
    store.write(doc)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckos_kernel::Capability;
    use ckos_memory::{InMemoryStore, Query};

    fn result(verified: bool, agent: Option<&str>, output: &str) -> ExecutionResult {
        ExecutionResult {
            task: TaskId::new(),
            capability: Capability::Reasoning,
            agent: agent.map(String::from),
            runtime: "echo".into(),
            output: output.into(),
            verified,
        }
    }

    #[test]
    fn scores_reflect_quality() {
        let r = HeuristicReflector::new();
        assert!(r.reflect(&result(false, Some("a"), "x")).score < 50);
        assert!(r.reflect(&result(true, None, "x")).score < 80);
        assert!(r.reflect(&result(true, Some("a"), "x")).score >= 90);
    }

    #[test]
    fn consensus_averages_and_dedupes_hints() {
        let reflections = vec![
            HeuristicReflector::new().reflect(&result(true, Some("a"), "x")),
            HeuristicReflector::new().reflect(&result(true, Some("b"), "y")),
            HeuristicReflector::new().reflect(&result(false, Some("c"), "z")),
        ];
        let c = consensus(&reflections);
        // (95 + 95 + 20) / 3 = 70
        assert_eq!(c.score, 70);
        // Two distinct hints (accepted x2, failed x1).
        assert_eq!(c.hints.len(), 2);
    }

    #[test]
    fn empty_consensus_is_zero() {
        assert_eq!(consensus(&[]).score, 0);
    }

    #[test]
    fn reflections_persist_to_memory() {
        let mut store = InMemoryStore::new();
        let reflection = HeuristicReflector::new().reflect(&result(false, None, ""));
        store_reflection(&mut store, &reflection).unwrap();
        let hits = store
            .search(&Query {
                doc_type: Some("reflection".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].confidence, reflection.score);
    }
}
