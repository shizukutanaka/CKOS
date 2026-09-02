//! Telemetry (§904).
//!
//! Collects per-task execution metrics — latency, tokens, derived token rate —
//! and aggregates them so the scheduler can optimise (§913): e.g. bias
//! `runtime_fit` toward the runtime with the best observed latency.
//!
//! Latency is stored in **nanoseconds** — the resolution `Instant` actually
//! provides. A local runtime serves a task in hundreds of nanoseconds, so
//! millisecond storage truncated every such sample to zero and the aggregates
//! then reported a *measured* 0 ms / 0 tok/s for work that had really run.
//! Rates are `Option`: `None` means "nothing to divide by", which is not the
//! same statement as a throughput of zero.

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
    /// Wall-clock latency in nanoseconds. Finer than the millisecond the
    /// display uses, because sub-millisecond work is still work.
    pub latency_ns: u64,
    /// Tokens produced.
    pub tokens: usize,
}

impl TaskMetrics {
    /// Latency in milliseconds, for display. Fractional: 83 µs is `0.083`.
    pub fn latency_ms(&self) -> f64 {
        self.latency_ns as f64 / 1_000_000.0
    }

    /// Tokens per second, or `None` when the elapsed time is too small to
    /// measure (a zero denominator). Reporting `0.0` there would state the
    /// opposite of the truth — that instant work was infinitely slow.
    pub fn tokens_per_sec(&self) -> Option<f64> {
        if self.latency_ns == 0 {
            None
        } else {
            Some(self.tokens as f64 * 1_000_000_000.0 / self.latency_ns as f64)
        }
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
        let sum: u64 = g.iter().map(|m| m.latency_ns).sum();
        Some(sum as f64 / g.len() as f64 / 1_000_000.0)
    }

    /// Mean latency **in nanoseconds** for a specific runtime, or `None` if it
    /// has no samples. Lets the scheduler bias `runtime_fit` toward faster
    /// runtimes (§913) — in the unit the samples are stored in, so two
    /// sub-millisecond runtimes remain distinguishable.
    pub fn mean_latency_ns_for(&self, runtime: &str) -> Option<f64> {
        let g = lock_recover(&self.samples);
        let matching: Vec<u64> = g
            .iter()
            .filter(|m| m.runtime == runtime)
            .map(|m| m.latency_ns)
            .collect();
        if matching.is_empty() {
            return None;
        }
        Some(matching.iter().sum::<u64>() as f64 / matching.len() as f64)
    }

    /// Aggregate tokens-per-second across all samples, or `None` when there is
    /// nothing to divide by (no samples, or no measurable elapsed time). Same
    /// contract as [`InMemoryTelemetry::mean_latency_ms`]: absence of data is
    /// reported as absence, not as the number zero.
    pub fn mean_tokens_per_sec(&self) -> Option<f64> {
        let g = lock_recover(&self.samples);
        let total_ns: u64 = g.iter().map(|m| m.latency_ns).sum();
        let total_tokens: usize = g.iter().map(|m| m.tokens).sum();
        if total_ns == 0 {
            None
        } else {
            Some(total_tokens as f64 * 1_000_000_000.0 / total_ns as f64)
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
            latency_ns: 500_000_000,
            tokens: 10,
        };
        assert_eq!(m.tokens_per_sec(), Some(20.0));
        assert_eq!(m.latency_ms(), 500.0);
    }

    #[test]
    fn sub_millisecond_work_is_measured_not_truncated() {
        // Regression: latency was stored in whole milliseconds, so a task that
        // really took 83 µs was recorded as 0 — and the aggregates then stated
        // a *measured* zero latency and zero throughput. Both are now real
        // numbers, and an unmeasurable sample says so instead of claiming 0.
        let m = TaskMetrics {
            runtime: "echo".into(),
            latency_ns: 83_000,
            tokens: 10,
        };
        assert!((m.latency_ms() - 0.083).abs() < 1e-9);
        let rate = m.tokens_per_sec().expect("83 µs is measurable");
        assert!(rate > 100_000.0, "10 tokens in 83 µs is fast: {rate}");

        let unmeasurable = TaskMetrics {
            runtime: "echo".into(),
            latency_ns: 0,
            tokens: 10,
        };
        assert_eq!(unmeasurable.tokens_per_sec(), None, "no denominator");

        // Two sub-millisecond runtimes stay distinguishable, which whole
        // milliseconds could not express (both would have been 0).
        let t = InMemoryTelemetry::new();
        t.record(TaskMetrics {
            runtime: "quick".into(),
            latency_ns: 4_000,
            tokens: 1,
        });
        t.record(TaskMetrics {
            runtime: "slower".into(),
            latency_ns: 830_000,
            tokens: 1,
        });
        assert_eq!(t.mean_latency_ns_for("quick"), Some(4_000.0));
        assert_eq!(t.mean_latency_ns_for("slower"), Some(830_000.0));
    }

    #[test]
    fn an_empty_aggregator_reports_absence_not_zero() {
        // `mean_latency_ms` always said `None` for "no samples"; the rate said
        // `0.0`, which reads as a measurement. They now agree.
        let t = InMemoryTelemetry::new();
        assert_eq!(t.mean_latency_ms(), None);
        assert_eq!(t.mean_tokens_per_sec(), None);
    }

    #[test]
    fn retention_is_bounded_drop_oldest() {
        let t = InMemoryTelemetry::new().with_max_samples(2);
        for latency in [1, 2, 3] {
            t.record(TaskMetrics {
                runtime: "echo".into(),
                latency_ns: latency * 1_000_000,
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
            latency_ns: 10_000_000,
            tokens: 5,
        });
        t.record(TaskMetrics {
            runtime: "slow".into(),
            latency_ns: 90_000_000,
            tokens: 5,
        });
        assert_eq!(t.len(), 2);
        assert_eq!(t.total_tokens(), 10);
        assert_eq!(t.mean_latency_ms(), Some(50.0));
        assert_eq!(t.mean_latency_ns_for("fast"), Some(10_000_000.0));
        assert_eq!(t.mean_latency_ns_for("missing"), None);
        // 10 tokens over 100 ms total.
        assert_eq!(t.mean_tokens_per_sec(), Some(100.0));
    }
}
