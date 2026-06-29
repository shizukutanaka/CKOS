//! Audit logging (§903).
//!
//! Every significant action is recorded as an [`AuditRecord`]: when it happened,
//! which runtime/tool/plugin was involved, content hashes of the input and
//! output (so the trail is verifiable without storing raw, possibly sensitive
//! payloads), and any error. Records flow to an [`AuditSink`]; this is the
//! **audit** channel and is deliberately distinct from ordinary debug logging,
//! which the spec asks to keep separate.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
#[derive(Clone, Default)]
pub struct InMemoryAuditLog {
    records: Arc<Mutex<Vec<AuditRecord>>>,
}

impl InMemoryAuditLog {
    /// Create an empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of records.
    pub fn len(&self) -> usize {
        self.records.lock().expect("audit log poisoned").len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Snapshot of all records, oldest first.
    pub fn snapshot(&self) -> Vec<AuditRecord> {
        self.records.lock().expect("audit log poisoned").clone()
    }

    /// Number of records that recorded an error.
    pub fn error_count(&self) -> usize {
        self.records
            .lock()
            .expect("audit log poisoned")
            .iter()
            .filter(|r| r.is_error())
            .count()
    }
}

impl AuditSink for InMemoryAuditLog {
    fn record(&self, record: AuditRecord) {
        self.records
            .lock()
            .expect("audit log poisoned")
            .push(record);
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
