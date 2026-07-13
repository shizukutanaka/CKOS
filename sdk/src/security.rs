//! Distributed security (§930): message signing and replay protection.
//!
//! Inter-agent [`Message`] values crossing a trust boundary
//! are wrapped in a [`SignedEnvelope`] carrying a nonce, a timestamp and a
//! signature over the canonical message bytes. A [`ReplayGuard`] verifies the
//! signature, rejects messages outside a freshness window, and rejects repeated
//! nonces — covering the "message signing", "replay attack" and (via the audit
//! log) "audit" items of §930. Seen nonces are pruned once their timestamp
//! ages out of the freshness window, so a long-running guard's memory stays
//! bounded by the window rather than growing for the process lifetime.
//!
//! The signing primitive here is a keyed hash built from the dependency-free
//! FNV-1a used elsewhere; it demonstrates the protocol but is **not**
//! cryptographically strong. A production deployment swaps [`sign`] for
//! HMAC-SHA256 and pairs it with the mTLS / certificate-rotation transport the
//! spec lists (those are transport concerns, layered below this module).

use crate::messaging::Message;
use ckos_kernel::audit::content_hash;

/// Keyed signature over `data`. Deterministic; not cryptographically strong.
pub fn sign(key: u64, data: &str) -> u64 {
    let base = content_hash(data);
    base.rotate_left(17) ^ key.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ content_hash(&key.to_string())
}

/// Canonical, signable encoding of a message plus its nonce and timestamp.
fn canonical(message: &Message, nonce: u64, timestamp_ms: u64) -> String {
    format!(
        "{}|{}|{}|{}|{:?}|{}|{}|{}|{}|{}",
        message.id,
        message.source,
        message.destination,
        message.msg_type,
        message.priority,
        message.payload.body,
        message
            .payload
            .graph_id
            .as_ref()
            .map(|g| g.as_str())
            .unwrap_or(""),
        message
            .payload
            .memory_ref
            .as_ref()
            .map(|m| m.as_str())
            .unwrap_or(""),
        nonce,
        timestamp_ms,
    )
}

/// A signed, replay-protected message envelope (§930).
#[derive(Debug, Clone)]
pub struct SignedEnvelope {
    /// The wrapped message.
    pub message: Message,
    /// Unique value preventing replay of an identical envelope.
    pub nonce: u64,
    /// Sealing time in milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
    /// Keyed digest over the message, nonce and timestamp.
    pub signature: u64,
}

/// Seals messages with a shared key.
pub struct Signer {
    key: u64,
}

impl Signer {
    /// Create a signer with the shared key.
    pub fn new(key: u64) -> Self {
        Signer { key }
    }

    /// Seal a message. Callers must supply a unique `nonce` and the current
    /// `timestamp_ms`; in production the nonce should be cryptographically
    /// random.
    pub fn seal(&self, message: Message, nonce: u64, timestamp_ms: u64) -> SignedEnvelope {
        let signature = sign(self.key, &canonical(&message, nonce, timestamp_ms));
        SignedEnvelope {
            message,
            nonce,
            timestamp_ms,
            signature,
        }
    }
}

/// Why verification failed (§930).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityError {
    /// Signature did not match — tampered or wrong key.
    BadSignature,
    /// Outside the freshness window — possible replay of a stale message.
    Expired,
    /// Nonce already seen — a replay.
    Replayed,
}

/// Verifies envelopes against a shared key, a freshness window and seen
/// nonces (nonce -> the timestamp it was sealed with, so aged-out entries can
/// be pruned; see [`verify`](Self::verify)).
pub struct ReplayGuard {
    key: u64,
    window_ms: u64,
    seen: std::collections::HashMap<u64, u64>,
}

impl ReplayGuard {
    /// Create a guard with the shared key and freshness window in milliseconds.
    pub fn new(key: u64, window_ms: u64) -> Self {
        ReplayGuard {
            key,
            window_ms,
            seen: std::collections::HashMap::new(),
        }
    }

    /// Nonces currently tracked for replay detection — a diagnostic seam
    /// proving [`verify`](Self::verify) actually bounds this, not an API a
    /// caller should branch on.
    pub fn tracked_nonces(&self) -> usize {
        self.seen.len()
    }

    /// Verify an envelope at time `now_ms`. On success the nonce is consumed so
    /// the same envelope cannot be accepted twice (§930 replay protection).
    pub fn verify(&mut self, envelope: &SignedEnvelope, now_ms: u64) -> Result<(), SecurityError> {
        let expected = sign(
            self.key,
            &canonical(&envelope.message, envelope.nonce, envelope.timestamp_ms),
        );
        if expected != envelope.signature {
            return Err(SecurityError::BadSignature);
        }
        if now_ms.abs_diff(envelope.timestamp_ms) > self.window_ms {
            return Err(SecurityError::Expired);
        }
        // Drop nonces that have aged out of the freshness window: a replay
        // carrying one of their timestamps would fail the `Expired` check
        // above before ever reaching the nonce lookup, so remembering them
        // any longer only grows unboundedly for no correctness benefit
        // (assumes `now_ms` is non-decreasing across calls, the normal
        // wall-clock case this guards against).
        let window_ms = self.window_ms;
        self.seen.retain(|_, ts| now_ms.abs_diff(*ts) <= window_ms);
        if self.seen.contains_key(&envelope.nonce) {
            return Err(SecurityError::Replayed);
        }
        self.seen.insert(envelope.nonce, envelope.timestamp_ms);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckos_kernel::AgentId;

    fn msg() -> Message {
        Message::new(AgentId::new(), AgentId::new(), "reasoning").with_body("hello")
    }

    #[test]
    fn valid_envelope_verifies_once() {
        let signer = Signer::new(0xDEAD_BEEF);
        let mut guard = ReplayGuard::new(0xDEAD_BEEF, 1000);
        let env = signer.seal(msg(), 1, 10_000);
        assert_eq!(guard.verify(&env, 10_200), Ok(()));
        // Same envelope again → replay.
        assert_eq!(guard.verify(&env, 10_300), Err(SecurityError::Replayed));
    }

    #[test]
    fn tampering_breaks_the_signature() {
        let signer = Signer::new(7);
        let mut guard = ReplayGuard::new(7, 1000);
        let mut env = signer.seal(msg(), 2, 10_000);
        env.message.payload.body = "tampered".into();
        assert_eq!(guard.verify(&env, 10_000), Err(SecurityError::BadSignature));
    }

    #[test]
    fn wrong_key_is_rejected() {
        let signer = Signer::new(1);
        let mut guard = ReplayGuard::new(2, 1000);
        let env = signer.seal(msg(), 3, 10_000);
        assert_eq!(guard.verify(&env, 10_000), Err(SecurityError::BadSignature));
    }

    #[test]
    fn stale_message_is_expired() {
        let signer = Signer::new(9);
        let mut guard = ReplayGuard::new(9, 1000);
        let env = signer.seal(msg(), 4, 10_000);
        // 2s later, window is 1s → expired.
        assert_eq!(guard.verify(&env, 12_001), Err(SecurityError::Expired));
    }

    #[test]
    fn aged_out_nonces_are_pruned_not_retained_forever() {
        // Before this fix, `seen` was a HashSet that never shrank — a
        // long-running guard's memory grew for the process lifetime. A nonce
        // whose timestamp has aged past the freshness window can never again
        // affect a verdict (a replay carrying it would hit `Expired` first),
        // so it must be evicted, keeping memory bounded by the window.
        let signer = Signer::new(5);
        let mut guard = ReplayGuard::new(5, 1000);
        for n in 0..5 {
            let env = signer.seal(msg(), n, 10_000);
            assert_eq!(guard.verify(&env, 10_000), Ok(()));
        }
        assert_eq!(guard.tracked_nonces(), 5);

        // Far enough past the window that all five have aged out; a sixth,
        // fresh nonce triggers the prune inside verify().
        let fresh = signer.seal(msg(), 99, 50_000);
        assert_eq!(guard.verify(&fresh, 50_000), Ok(()));
        assert_eq!(guard.tracked_nonces(), 1, "stale nonces should be pruned");
    }
}
