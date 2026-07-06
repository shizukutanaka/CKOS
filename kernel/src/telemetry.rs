//! Telemetry (§904).
//!
//! Collects per-task execution metrics — latency, tokens, derived token rate —
//! and aggregates them so the scheduler can optimise (§913): e.g. bias
//! `runtime_fit` toward the runtime with the best observed latency. Hardware
//! counters (CPU/GPU/NPU/memory/power) are exposed through the [`ResourceProbe`]
//! seam; the default [`NullProbe`] reports nothing so the core stays dependency
//! free, and a platform probe plugs in without touching callers.

use std::sync::{Arc, Mutex, MutexGuard};

/// Take a lock, recovering from poisoning: telemetry state is a plain
/// append-only Vec with no partial-update invariants, so if another thread
/// panicked while holding the lock the data inside is still coherent.
/// Propagating the poison would turn one unrelated panic into a permanent
/// cascade where every later telemetry call also panics (§904 must degrade,
/// not amplify).
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Default cap on retained samples; see [`InMemoryTelemetry::with_max_samples`].
const DEFAULT_MAX_SAMPLES: usize = 10_000;

/// Metrics for a single task execution (§904).
#[derive(Debug, Clone)]
pub struct TaskMetrics {
    /// Runtime that served the task (§900).
    pub runtime: String,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: u64,
    /// Tokens produced.
    pub tokens: usize,
}

impl TaskMetrics {
    /// Tokens per second; 0.0 when latency is unknown.
    pub fn tokens_per_sec(&self) -> f64 {
        if self.latency_ms == 0 {
            0.0
        } else {
            self.tokens as f64 * 1000.0 / self.latency_ms as f64
        }
    }
}

/// A point-in-time hardware resource sample (§904). Fields are fractions
/// `0.0..=1.0` for utilisations; `None` when a counter is unavailable.
#[derive(Debug, Clone, Default)]
pub struct ResourceSnapshot {
    /// CPU utilisation fraction, if available.
    pub cpu: Option<f32>,
    /// GPU utilisation fraction, if available.
    pub gpu: Option<f32>,
    /// NPU utilisation fraction, if available.
    pub npu: Option<f32>,
    /// Resident memory usage in megabytes, if available.
    pub memory_mb: Option<u64>,
    /// Instantaneous power draw in watts, if available.
    pub power_watts: Option<f32>,
}

/// Source of hardware resource samples (§904).
pub trait ResourceProbe: Send + Sync {
    /// Take a sample; may return an empty snapshot if counters are unavailable.
    fn sample(&self) -> ResourceSnapshot;
}

/// A probe that reports nothing — the dependency-free default.
#[derive(Default)]
pub struct NullProbe;

impl ResourceProbe for NullProbe {
    fn sample(&self) -> ResourceSnapshot {
        ResourceSnapshot::default()
    }
}

/// Destination for task metrics. Plug in Prometheus/OpenTelemetry here (§933).
pub trait TelemetrySink: Send + Sync {
    /// Record one task's metrics.
    fn record(&self, metrics: TaskMetrics);
}

/// In-memory, thread-safe telemetry aggregator. Cheap to clone (shared store).
///
/// Retention is bounded (drop-oldest, default 10 000 samples) so a long-lived
/// process cannot grow memory without limit; recent samples are also the ones
/// scheduling decisions should weigh (§913).
#[derive(Clone)]
pub struct InMemoryTelemetry {
    samples: Arc<Mutex<Vec<TaskMetrics>>>,
    max_samples: usize,
}

impl Default for InMemoryTelemetry {
    fn default() -> Self {
        InMemoryTelemetry {
            samples: Arc::default(),
            max_samples: DEFAULT_MAX_SAMPLES,
        }
    }
}

impl InMemoryTelemetry {
    /// Create an empty aggregator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cap retained samples at `max` (minimum 1); when full, the oldest
    /// sample is dropped to admit the newest.
    pub fn with_max_samples(mut self, max: usize) -> Self {
        self.max_samples = max.max(1);
        self
    }

    /// Number of recorded samples.
    pub fn len(&self) -> usize {
        lock_recover(&self.samples).len()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total tokens produced across all samples.
    pub fn total_tokens(&self) -> usize {
        lock_recover(&self.samples).iter().map(|m| m.tokens).sum()
    }

    /// Mean latency in milliseconds, or `None` if there are no samples.
    pub fn mean_latency_ms(&self) -> Option<f64> {
        let g = lock_recover(&self.samples);
        if g.is_empty() {
            return None;
        }
        let sum: u64 = g.iter().map(|m| m.latency_ms).sum();
        Some(sum as f64 / g.len() as f64)
    }

    /// Mean latency for a specific runtime, or `None` if it has no samples.
    /// Lets the scheduler bias `runtime_fit` toward faster runtimes (§913).
    pub fn mean_latency_for(&self, runtime: &str) -> Option<f64> {
        let g = lock_recover(&self.samples);
        let matching: Vec<u64> = g
            .iter()
            .filter(|m| m.runtime == runtime)
            .map(|m| m.latency_ms)
            .collect();
        if matching.is_empty() {
            return None;
        }
        Some(matching.iter().sum::<u64>() as f64 / matching.len() as f64)
    }

    /// Aggregate tokens-per-second across all samples.
    pub fn mean_tokens_per_sec(&self) -> f64 {
        let g = lock_recover(&self.samples);
        let total_ms: u64 = g.iter().map(|m| m.latency_ms).sum();
        let total_tokens: usize = g.iter().map(|m| m.tokens).sum();
        if total_ms == 0 {
            0.0
        } else {
            total_tokens as f64 * 1000.0 / total_ms as f64
        }
    }
}

impl TelemetrySink for InMemoryTelemetry {
    fn record(&self, metrics: TaskMetrics) {
        let mut g = lock_recover(&self.samples);
        g.push(metrics);
        let len = g.len();
        if len > self.max_samples {
            g.drain(..len - self.max_samples);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_rate_is_computed() {
        let m = TaskMetrics {
            runtime: "echo".into(),
            latency_ms: 500,
            tokens: 10,
        };
        assert_eq!(m.tokens_per_sec(), 20.0);
    }

    #[test]
    fn retention_is_bounded_drop_oldest() {
        let t = InMemoryTelemetry::new().with_max_samples(2);
        for latency in [1, 2, 3] {
            t.record(TaskMetrics {
                runtime: "echo".into(),
                latency_ms: latency,
                tokens: 1,
            });
        }
        assert_eq!(t.len(), 2, "capped at max_samples");
        // The oldest sample (1ms) was dropped: mean over the survivors only.
        assert_eq!(t.mean_latency_ms(), Some(2.5));
    }

    #[test]
    fn aggregates_per_runtime_and_overall() {
        let t = InMemoryTelemetry::new();
        t.record(TaskMetrics {
            runtime: "fast".into(),
            latency_ms: 10,
            tokens: 5,
        });
        t.record(TaskMetrics {
            runtime: "slow".into(),
            latency_ms: 90,
            tokens: 5,
        });
        assert_eq!(t.len(), 2);
        assert_eq!(t.total_tokens(), 10);
        assert_eq!(t.mean_latency_ms(), Some(50.0));
        assert_eq!(t.mean_latency_for("fast"), Some(10.0));
        assert_eq!(t.mean_latency_for("missing"), None);
    }
}
