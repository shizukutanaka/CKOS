//! Audit logging (§903).
//!
//! Every significant action is recorded as an [`AuditRecord`]: when it happened,
//! which runtime/tool/plugin was involved, content hashes of the input and
//! output (so the trail is verifiable without storing raw, possibly sensitive
//! payloads), and any error. Records flow to an [`AuditSink`]; this is the
//! **audit** channel and is deliberately distinct from ordinary debug logging,
//! which the spec asks to keep separate.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Take a lock, recovering from poisoning: the audit log is a plain
/// append-only Vec with no partial-update invariants, so the data is coherent
/// even if another thread panicked while holding the lock. Propagating the
/// poison would make every later audit write panic too — and an audit-trail
/// failure must never abort work that already succeeded (§903).
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Default cap on retained records; see [`InMemoryAuditLog::with_max_records`].
const DEFAULT_MAX_RECORDS: usize = 10_000;

/// FNV-1a 64-bit content hash. Lets the audit trail prove what was processed
/// without retaining the raw bytes.
pub fn content_hash(data: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A single audit entry (§903).
#[derive(Debug, Clone)]
pub struct AuditRecord {
    /// Unix epoch milliseconds when the action completed.
    pub timestamp_ms: u64,
    /// Action label, e.g. `task.execute`.
    pub action: String,
    /// Runtime that served the action, if any (§900).
    pub runtime: Option<String>,
    /// Tool invoked, if any (§917).
    pub tool: Option<String>,
    /// Plugin involved, if any (§901).
    pub plugin: Option<String>,
    /// Hash of the input payload.
    pub input_hash: u64,
    /// Hash of the output payload.
    pub output_hash: u64,
    /// Error message if the action failed.
    pub error: Option<String>,
}

impl AuditRecord {
    /// Start a record for `action`, stamped with the current time.
    pub fn new(action: impl Into<String>) -> Self {
        AuditRecord {
            timestamp_ms: now_millis(),
            action: action.into(),
            runtime: None,
            tool: None,
            plugin: None,
            input_hash: 0,
            output_hash: 0,
            error: None,
        }
    }

    /// Set the runtime.
    pub fn runtime(mut self, name: impl Into<String>) -> Self {
        self.runtime = Some(name.into());
        self
    }

    /// Set the tool.
    pub fn tool(mut self, name: impl Into<String>) -> Self {
        self.tool = Some(name.into());
        self
    }

    /// Set the plugin.
    pub fn plugin(mut self, name: impl Into<String>) -> Self {
        self.plugin = Some(name.into());
        self
    }

    /// Hash and record the input payload.
    pub fn input(mut self, data: &str) -> Self {
        self.input_hash = content_hash(data);
        self
    }

    /// Hash and record the output payload.
    pub fn output(mut self, data: &str) -> Self {
        self.output_hash = content_hash(data);
        self
    }

    /// Record an error.
    pub fn error(mut self, msg: impl Into<String>) -> Self {
        self.error = Some(msg.into());
        self
    }

    /// Whether this record represents a failure.
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// Destination for audit records. Implement this to forward to a file, SIEM, or
/// OpenTelemetry exporter (§933).
pub trait AuditSink: Send + Sync {
    /// Persist one record.
    fn record(&self, record: AuditRecord);
}

/// An in-memory, thread-safe audit log. Cheap to clone (shares storage), so it
/// can be handed to subsystems while remaining inspectable.
///
/// Retention is bounded (drop-oldest, default 10 000 records) so a long-lived
/// process cannot grow memory without limit; a durable [`AuditSink`] (file,
/// SIEM) is where a full-history trail belongs.
#[derive(Clone)]
pub struct InMemoryAuditLog {
    records: Arc<Mutex<Vec<AuditRecord>>>,
    max_records: usize,
}

impl Default for InMemoryAuditLog {
    fn default() -> Self {
        InMemoryAuditLog {
            records: Arc::default(),
            max_records: DEFAULT_MAX_RECORDS,
        }
    }
}

impl InMemoryAuditLog {
    /// Create an empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cap retained records at `max` (minimum 1); when full, the oldest
    /// record is dropped to admit the newest.
    pub fn with_max_records(mut self, max: usize) -> Self {
        self.max_records = max.max(1);
        self
    }

    /// Number of records.
    pub fn len(&self) -> usize {
        lock_recover(&self.records).len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Snapshot of all records, oldest first.
    pub fn snapshot(&self) -> Vec<AuditRecord> {
        lock_recover(&self.records).clone()
    }

    /// Number of records that recorded an error.
    pub fn error_count(&self) -> usize {
        lock_recover(&self.records)
            .iter()
            .filter(|r| r.is_error())
            .count()
    }
}

impl AuditSink for InMemoryAuditLog {
    fn record(&self, record: AuditRecord) {
        let mut g = lock_recover(&self.records);
        g.push(record);
        let len = g.len();
        if len > self.max_records {
            g.drain(..len - self.max_records);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_and_distinguishing() {
        assert_eq!(content_hash("abc"), content_hash("abc"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
    }

    #[test]
    fn hash_matches_the_published_fnv_1a_vectors() {
        // The audit trail's whole claim is that a recorded hash proves what was
        // processed without keeping the payload (§903). That only holds if the
        // hash is stable *across builds* — hashes recorded yesterday must still
        // match the same input today. Determinism within one process cannot
        // catch a changed constant; only known answers can. Pinned against the
        // published Fowler–Noll–Vo FNV-1a 64-bit reference vectors, the same
        // "verify against the standard, not against ourselves" rule already
        // applied to SHA-256/HMAC in `ckos_sdk::crypto`.
        assert_eq!(content_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(content_hash("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(content_hash("foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn survives_a_panic_while_the_lock_is_held() {
        // A thread that panics while holding the audit lock poisons it; the
        // log must keep working afterwards instead of cascading the panic
        // into every later record/snapshot call.
        let log = InMemoryAuditLog::new();
        log.record(AuditRecord::new("before"));

        let cloned = log.clone();
        let _ = std::thread::spawn(move || {
            let _guard = lock_recover(&cloned.records);
            panic!("poison the lock deliberately");
        })
        .join(); // the Err result is the expected panic

        log.record(AuditRecord::new("after"));
        let snap = log.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[1].action, "after");
    }

    #[test]
    fn retention_is_bounded_drop_oldest() {
        let log = InMemoryAuditLog::new().with_max_records(3);
        for i in 0..5 {
            log.record(AuditRecord::new(format!("a{i}")));
        }
        let snap = log.snapshot();
        assert_eq!(snap.len(), 3, "capped at max_records");
        // The oldest records were dropped; the newest survive in order.
        let actions: Vec<&str> = snap.iter().map(|r| r.action.as_str()).collect();
        assert_eq!(actions, ["a2", "a3", "a4"]);
    }

    #[test]
    fn log_records_and_counts_errors() {
        let log = InMemoryAuditLog::new();
        log.record(
            AuditRecord::new("task.execute")
                .runtime("echo")
                .input("x")
                .output("x"),
        );
        log.record(
            AuditRecord::new("task.execute")
                .runtime("echo")
                .error("boom"),
        );
        assert_eq!(log.len(), 2);
        assert_eq!(log.error_count(), 1);
        let snap = log.snapshot();
        assert_eq!(snap[0].runtime.as_deref(), Some("echo"));
        assert!(snap[1].is_error());
    }
}
