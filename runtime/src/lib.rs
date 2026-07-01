//! # CKOS Runtime Abstraction Layer
//!
//! The Runtime Abstraction Layer (RAL) sits between the kernel and concrete
//! inference engines (§889). The kernel selects a runtime through the
//! [`RuntimeRegistry`] (§900) and pool (§924); it never links a specific engine
//! directly, so a new backend is just another [`Runtime`] implementation or a
//! plugin (§901).

use ckos_kernel::capability::Capability;
use ckos_kernel::error::{KernelError, Result};
use ckos_kernel::RuntimeId;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Where a runtime executes (§924, §925).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    /// Local CPU execution (e.g. llama.cpp).
    Cpu,
    /// Local GPU execution (e.g. vLLM, DirectML).
    Gpu,
    /// Neural accelerator (e.g. OpenVINO/NPU).
    Npu,
    /// On-device edge runtime, usable while offline (§925).
    Edge,
    /// Remote cluster / cloud runtime.
    Cloud,
}

impl RuntimeKind {
    /// Canonical lowercase token.
    pub fn as_token(&self) -> &'static str {
        match self {
            RuntimeKind::Cpu => "cpu",
            RuntimeKind::Gpu => "gpu",
            RuntimeKind::Npu => "npu",
            RuntimeKind::Edge => "edge",
            RuntimeKind::Cloud => "cloud",
        }
    }
}

impl fmt::Display for RuntimeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_token())
    }
}

impl FromStr for RuntimeKind {
    type Err = KernelError;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cpu" => Ok(RuntimeKind::Cpu),
            "gpu" => Ok(RuntimeKind::Gpu),
            "npu" => Ok(RuntimeKind::Npu),
            "edge" => Ok(RuntimeKind::Edge),
            "cloud" => Ok(RuntimeKind::Cloud),
            other => Err(KernelError::other(format!("unknown runtime kind: {other}"))),
        }
    }
}

/// A request handed to a runtime. Kept engine-agnostic on purpose.
#[derive(Debug, Clone)]
pub struct InferenceRequest {
    /// The prompt or serialized input payload.
    pub input: String,
    /// Capability the request needs (used for routing/validation).
    pub capability: Capability,
    /// Soft token budget; engines may ignore it.
    pub max_tokens: usize,
}

/// A response produced by a runtime.
#[derive(Debug, Clone)]
pub struct InferenceResponse {
    /// The produced text/output payload.
    pub output: String,
    /// Tokens generated, for telemetry (§904).
    pub tokens: usize,
}

/// A pluggable inference backend (§889, §900).
pub trait Runtime: Send + Sync {
    /// Stable identifier of this runtime instance.
    fn id(&self) -> &RuntimeId;
    /// Human-readable name (e.g. "llama.cpp").
    fn name(&self) -> &str;
    /// Execution locality.
    fn kind(&self) -> RuntimeKind;
    /// Capabilities this runtime can serve.
    fn capabilities(&self) -> &[Capability];
    /// Whether this runtime can serve `cap`.
    fn supports(&self, cap: &Capability) -> bool {
        self.capabilities().contains(cap)
    }
    /// Execute a request synchronously.
    fn run(&self, req: &InferenceRequest) -> Result<InferenceResponse>;
}

/// Locality preference used by [`RuntimeRegistry::select`]: lower is more
/// preferred. Local/offline-capable kinds rank ahead of remote ones (§925).
fn locality_rank(kind: RuntimeKind) -> u8 {
    match kind {
        RuntimeKind::Edge => 0,
        RuntimeKind::Npu => 1,
        RuntimeKind::Gpu => 2,
        RuntimeKind::Cpu => 3,
        RuntimeKind::Cloud => 4,
    }
}

/// Descriptor stored in the registry table (§900).
#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    pub id: RuntimeId,
    pub name: String,
    pub kind: RuntimeKind,
    pub capabilities: Vec<Capability>,
}

/// Registry + pool of runtimes (§900, §924).
///
/// `select` implements the kernel's "pick the best runtime" policy: prefer a
/// runtime that supports the capability, biased toward local execution so the
/// system degrades gracefully offline (§925).
#[derive(Default)]
pub struct RuntimeRegistry {
    runtimes: HashMap<RuntimeId, Box<dyn Runtime>>,
    order: Vec<RuntimeId>,
}

impl RuntimeRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a runtime, returning its id.
    pub fn register(&mut self, runtime: Box<dyn Runtime>) -> RuntimeId {
        let id = runtime.id().clone();
        self.order.push(id.clone());
        self.runtimes.insert(id.clone(), runtime);
        id
    }

    /// List descriptors for all registered runtimes (§900 table).
    pub fn list(&self) -> Vec<RuntimeInfo> {
        self.order
            .iter()
            .filter_map(|id| self.runtimes.get(id))
            .map(|r| RuntimeInfo {
                id: r.id().clone(),
                name: r.name().to_string(),
                kind: r.kind(),
                capabilities: r.capabilities().to_vec(),
            })
            .collect()
    }

    /// Borrow a runtime by id.
    pub fn get(&self, id: &RuntimeId) -> Option<&dyn Runtime> {
        self.runtimes.get(id).map(|b| b.as_ref())
    }

    /// Select the best runtime for a capability (§924): among runtimes that
    /// support it, the most local kind wins (`Edge` first, `Cloud` last, per
    /// [`locality_rank`] — lower rank = more preferred = selected), so the
    /// kernel favours offline-capable execution (§925). Ties (same rank,
    /// multiple runtimes) resolve to the first match in registration order —
    /// stable and deterministic, not round-robin or random.
    pub fn select(&self, cap: &Capability) -> Result<&dyn Runtime> {
        self.order
            .iter()
            .filter_map(|id| self.runtimes.get(id))
            .filter(|r| r.supports(cap))
            .min_by_key(|r| locality_rank(r.kind()))
            .map(|b| b.as_ref())
            .ok_or_else(|| KernelError::CapabilityUnavailable(cap.to_string()))
    }
}

/// A trivial echo runtime, useful for tests and offline smoke checks.
pub struct EchoRuntime {
    id: RuntimeId,
    caps: Vec<Capability>,
}

impl EchoRuntime {
    /// Create an echo runtime advertising the given capabilities.
    pub fn new(caps: Vec<Capability>) -> Self {
        EchoRuntime {
            id: RuntimeId::new(),
            caps,
        }
    }
}

impl Runtime for EchoRuntime {
    fn id(&self) -> &RuntimeId {
        &self.id
    }
    fn name(&self) -> &str {
        "echo"
    }
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Cpu
    }
    fn capabilities(&self) -> &[Capability] {
        &self.caps
    }
    fn run(&self, req: &InferenceRequest) -> Result<InferenceResponse> {
        Ok(InferenceResponse {
            output: req.input.clone(),
            tokens: req.input.split_whitespace().count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_kind_display_fromstr_round_trip() {
        for k in [
            RuntimeKind::Cpu,
            RuntimeKind::Gpu,
            RuntimeKind::Npu,
            RuntimeKind::Edge,
            RuntimeKind::Cloud,
        ] {
            assert_eq!(k.to_string().parse::<RuntimeKind>().unwrap(), k);
        }
        assert!("quantum".parse::<RuntimeKind>().is_err());
    }

    #[test]
    fn selects_supporting_runtime() {
        let mut reg = RuntimeRegistry::new();
        reg.register(Box::new(EchoRuntime::new(vec![Capability::Embedding])));
        let rt = reg.select(&Capability::Embedding).unwrap();
        assert_eq!(rt.name(), "echo");
        assert!(reg.select(&Capability::Vision).is_err());
    }

    /// A runtime whose kind is configurable, so locality preference can be
    /// tested across every [`RuntimeKind`] (`EchoRuntime` is always `Cpu`).
    struct KindedRuntime {
        id: RuntimeId,
        kind: RuntimeKind,
        name: &'static str,
        caps: Vec<Capability>,
    }
    impl Runtime for KindedRuntime {
        fn id(&self) -> &RuntimeId {
            &self.id
        }
        fn name(&self) -> &str {
            self.name
        }
        fn kind(&self) -> RuntimeKind {
            self.kind
        }
        fn capabilities(&self) -> &[Capability] {
            &self.caps
        }
        fn run(&self, req: &InferenceRequest) -> Result<InferenceResponse> {
            Ok(InferenceResponse {
                output: req.input.clone(),
                tokens: 0,
            })
        }
    }

    #[test]
    fn prefers_more_local_runtimes_over_remote_ones() {
        // Register Cloud, Cpu and Edge (out of locality order) all supporting
        // the same capability; select() must pick the most local one, Edge,
        // regardless of registration order.
        let mut reg = RuntimeRegistry::new();
        for (name, kind) in [
            ("cloud", RuntimeKind::Cloud),
            ("cpu", RuntimeKind::Cpu),
            ("edge", RuntimeKind::Edge),
            ("gpu", RuntimeKind::Gpu),
        ] {
            reg.register(Box::new(KindedRuntime {
                id: RuntimeId::new(),
                kind,
                name,
                caps: vec![Capability::Reasoning],
            }));
        }
        assert_eq!(reg.select(&Capability::Reasoning).unwrap().name(), "edge");
    }

    #[test]
    fn ties_resolve_to_first_registered() {
        // Two runtimes of the same (best) kind: selection is stable and
        // deterministic, picking the one registered first.
        let mut reg = RuntimeRegistry::new();
        reg.register(Box::new(KindedRuntime {
            id: RuntimeId::new(),
            kind: RuntimeKind::Edge,
            name: "edge-1",
            caps: vec![Capability::Coding],
        }));
        reg.register(Box::new(KindedRuntime {
            id: RuntimeId::new(),
            kind: RuntimeKind::Edge,
            name: "edge-2",
            caps: vec![Capability::Coding],
        }));
        assert_eq!(reg.select(&Capability::Coding).unwrap().name(), "edge-1");
    }

    #[test]
    fn echo_runs() {
        let rt = EchoRuntime::new(vec![Capability::Reasoning]);
        let resp = rt
            .run(&InferenceRequest {
                input: "hello world".into(),
                capability: Capability::Reasoning,
                max_tokens: 16,
            })
            .unwrap();
        assert_eq!(resp.output, "hello world");
        assert_eq!(resp.tokens, 2);
    }
}
