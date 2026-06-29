//! Agent manifests, lifecycle and the capability registry.
//!
//! An agent is the OS-level execution unit of CKOS — analogous to a process
//! (§907). Its [`AgentManifest`] (§908) declares the capabilities it provides,
//! the memory tiers and runtimes it needs, and the permissions it requests.
//! [`AgentState`] models the service lifecycle (§909) and [`CapabilityRegistry`]
//! lets workflows discover agents *by capability* (§910), not by name.

use ckos_kernel::capability::Capability;
use ckos_kernel::error::{KernelError, Result};
use ckos_kernel::task::Priority;
use ckos_kernel::AgentId;
use std::collections::HashMap;

/// Declarative metadata describing an agent (§908).
#[derive(Debug, Clone)]
pub struct AgentManifest {
    pub id: String,
    pub version: String,
    pub description: String,
    /// Capabilities this agent provides (§910, §911).
    pub capabilities: Vec<Capability>,
    /// Runtimes the agent can run on, by name (§908).
    pub runtimes: Vec<String>,
    /// Permission tokens the agent requests (§908, §919).
    pub permissions: Vec<String>,
    /// Scheduling priority hint (§913).
    pub priority: Priority,
}

impl AgentManifest {
    /// Construct a minimal manifest for an agent providing one capability.
    pub fn new(id: impl Into<String>, capability: Capability) -> Self {
        AgentManifest {
            id: id.into(),
            version: "0.1.0".into(),
            description: String::new(),
            capabilities: vec![capability],
            runtimes: Vec::new(),
            permissions: Vec::new(),
            priority: Priority::Normal,
        }
    }
}

/// Agent lifecycle states (§909).
///
/// ```text
/// Install -> Register -> Load -> Ready -> Running -> Suspended -> Resume -> Terminate
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Installed,
    Registered,
    Loaded,
    Ready,
    Running,
    Suspended,
    Terminated,
}

impl AgentState {
    /// Validate a lifecycle transition (§909).
    pub fn can_transition_to(self, next: AgentState) -> bool {
        use AgentState::*;
        matches!(
            (self, next),
            (Installed, Registered)
                | (Registered, Loaded)
                | (Loaded, Ready)
                | (Ready, Running)
                | (Running, Suspended)
                | (Running, Terminated)
                | (Suspended, Ready)      // Resume
                | (Suspended, Terminated)
                | (Ready, Terminated)
        )
    }
}

/// A registered, running agent instance.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    pub instance_id: AgentId,
    pub manifest: AgentManifest,
    pub state: AgentState,
}

/// Registry that indexes agents by capability for discovery (§910, §912).
#[derive(Default)]
pub struct CapabilityRegistry {
    agents: HashMap<AgentId, AgentInstance>,
    /// Capability token -> agent instance ids providing it.
    by_capability: HashMap<String, Vec<AgentId>>,
}

impl CapabilityRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent from its manifest, indexing every capability.
    pub fn register(&mut self, manifest: AgentManifest) -> AgentId {
        let id = AgentId::new();
        for cap in &manifest.capabilities {
            self.by_capability
                .entry(cap.as_token().to_string())
                .or_default()
                .push(id.clone());
        }
        self.agents.insert(
            id.clone(),
            AgentInstance {
                instance_id: id.clone(),
                manifest,
                state: AgentState::Registered,
            },
        );
        id
    }

    /// All agent instances providing a capability (§910).
    pub fn discover(&self, cap: &Capability) -> Vec<&AgentInstance> {
        self.by_capability
            .get(cap.as_token())
            .into_iter()
            .flatten()
            .filter_map(|id| self.agents.get(id))
            .collect()
    }

    /// Drive an agent's lifecycle, validating the transition (§909).
    pub fn transition(&mut self, id: &AgentId, next: AgentState) -> Result<()> {
        let agent = self
            .agents
            .get_mut(id)
            .ok_or_else(|| KernelError::NotFound(format!("agent {id}")))?;
        if !agent.state.can_transition_to(next) {
            return Err(KernelError::InvalidTransition {
                from: "agent-state",
                to: "agent-state",
            });
        }
        agent.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_agents_by_capability_not_name() {
        let mut reg = CapabilityRegistry::new();
        reg.register(AgentManifest::new("planner-a", Capability::Planning));
        reg.register(AgentManifest::new("planner-b", Capability::Planning));
        reg.register(AgentManifest::new("coder", Capability::Coding));

        assert_eq!(reg.discover(&Capability::Planning).len(), 2);
        assert_eq!(reg.discover(&Capability::Coding).len(), 1);
        assert_eq!(reg.discover(&Capability::Vision).len(), 0);
    }

    #[test]
    fn lifecycle_transitions_are_validated() {
        let mut reg = CapabilityRegistry::new();
        let id = reg.register(AgentManifest::new("x", Capability::Reasoning));
        reg.transition(&id, AgentState::Loaded).unwrap();
        reg.transition(&id, AgentState::Ready).unwrap();
        reg.transition(&id, AgentState::Running).unwrap();
        // Illegal jump.
        assert!(reg.transition(&id, AgentState::Installed).is_err());
    }
}
