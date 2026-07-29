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
//! The signing primitive is **HMAC-SHA256** ([`crate::crypto`], RFC 2104 over
//! FIPS 180-4), so a signature is unforgeable without the key. It replaces an
//! earlier keyed FNV-1a construction that was not merely "weak" but linear:
//! because that scheme XOR-mixed a key-only constant into a hash of the data,
//! the constant cancelled between any two signatures, letting an attacker who
//! observed **one** signed envelope compute a valid signature for **any**
//! message without ever learning the key. Signatures are compared in constant
//! time ([`crate::crypto::ct_eq`]) so a wrong tag leaks nothing through timing.
//!
//! Transport security (mTLS, certificate rotation) is layered below this
//! module and remains a deployment concern.

use crate::crypto::{ct_eq, hmac_sha256};
use crate::messaging::Message;

/// The width of a signature in bytes (a full HMAC-SHA256 tag).
pub const SIGNATURE_LEN: usize = 32;

/// HMAC-SHA256 signature over `data` under arbitrary-length key material.
///
/// Prefer this over the `u64` convenience keys when you control the key
/// material: a 64-bit shared secret has only 64 bits of entropy regardless of
/// how strong the MAC is.
pub fn sign(key: &[u8], data: &str) -> [u8; SIGNATURE_LEN] {
    hmac_sha256(key, data.as_bytes())
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
    /// HMAC-SHA256 tag over the message, nonce and timestamp.
    pub signature: [u8; SIGNATURE_LEN],
}

/// Seals messages with a shared key.
pub struct Signer {
    key: Vec<u8>,
}

impl Signer {
    /// Create a signer from a `u64` shared key (its big-endian bytes).
    pub fn new(key: u64) -> Self {
        Signer::with_key_bytes(key.to_be_bytes())
    }

    /// Create a signer from arbitrary-length key material — the form to use
    /// for real deployments, where a 64-bit secret is too little entropy.
    pub fn with_key_bytes(key: impl Into<Vec<u8>>) -> Self {
        Signer { key: key.into() }
    }

    /// Seal a message. Callers must supply a unique `nonce` and the current
    /// `timestamp_ms`; in production the nonce should be cryptographically
    /// random.
    pub fn seal(&self, message: Message, nonce: u64, timestamp_ms: u64) -> SignedEnvelope {
        let signature = sign(&self.key, &canonical(&message, nonce, timestamp_ms));
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
    key: Vec<u8>,
    window_ms: u64,
    seen: std::collections::HashMap<u64, u64>,
}

impl ReplayGuard {
    /// Create a guard from a `u64` shared key (its big-endian bytes) and a
    /// freshness window in milliseconds.
    pub fn new(key: u64, window_ms: u64) -> Self {
        ReplayGuard::with_key_bytes(key.to_be_bytes(), window_ms)
    }

    /// Create a guard from arbitrary-length key material — the counterpart to
    /// [`Signer::with_key_bytes`].
    pub fn with_key_bytes(key: impl Into<Vec<u8>>, window_ms: u64) -> Self {
        ReplayGuard {
            key: key.into(),
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
            &self.key,
            &canonical(&envelope.message, envelope.nonce, envelope.timestamp_ms),
        );
        // Constant-time: an `==` here would return on the first differing byte,
        // leaking through timing how much of a forged tag was correct.
        if !ct_eq(&expected, &envelope.signature) {
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
    fn observing_one_signature_does_not_let_an_attacker_forge_another() {
        // Regression for a total break of the old keyed-FNV primitive. It
        // computed `sign(key, d) = FNV(d).rot17 ^ K`, where `K` depended only
        // on the key — so `K` cancelled in the XOR of any two signatures and
        // an attacker who saw ONE envelope could compute the valid signature
        // for ANY other message, with no key and no brute force:
        //     forged = observed_sig ^ FNV(observed).rot17 ^ FNV(target).rot17
        // HMAC-SHA256 is not linear this way, so the same derivation now
        // yields a tag the guard rejects.
        fn fnv(data: &str) -> u64 {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in data.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        }

        let secret = 0xDEAD_BEEF_1234_5678u64;
        let signer = Signer::new(secret);
        let mut guard = ReplayGuard::new(secret, 10_000);

        let honest = signer.seal(msg().with_body("transfer 10"), 1, 10_000);
        assert_eq!(guard.verify(&honest, 10_000), Ok(()));

        // The attacker knows only the observed envelope — never `secret`.
        let mut forged = honest.clone();
        forged.message.payload.body = "transfer 1000000".into();
        forged.nonce = 2;
        let observed_canon = canonical(&honest.message, honest.nonce, honest.timestamp_ms);
        let target_canon = canonical(&forged.message, forged.nonce, forged.timestamp_ms);
        let delta = (fnv(&observed_canon).rotate_left(17) ^ fnv(&target_canon).rotate_left(17))
            .to_be_bytes();
        for (i, b) in delta.iter().enumerate() {
            forged.signature[i] ^= b;
        }

        assert_eq!(
            guard.verify(&forged, 10_000),
            Err(SecurityError::BadSignature),
            "a signature must not be derivable from another signature"
        );
    }

    #[test]
    fn arbitrary_length_key_material_round_trips() {
        // A 64-bit shared secret is only 64 bits of entropy however strong the
        // MAC is; real deployments pass full key material instead.
        let key = b"a much longer shared secret than sixty-four bits of entropy";
        let signer = Signer::with_key_bytes(&key[..]);
        let mut guard = ReplayGuard::with_key_bytes(&key[..], 1000);
        let env = signer.seal(msg(), 11, 10_000);
        assert_eq!(guard.verify(&env, 10_000), Ok(()));

        // A different key rejects the same envelope.
        let mut other = ReplayGuard::with_key_bytes(&b"different key material"[..], 1000);
        assert_eq!(other.verify(&env, 10_000), Err(SecurityError::BadSignature));
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
