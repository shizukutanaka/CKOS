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
    /// Memory tiers the agent needs, by name e.g. `working`, `graph` (§908).
    pub memory: Vec<String>,
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
            memory: Vec::new(),
            runtimes: Vec::new(),
            permissions: Vec::new(),
            priority: Priority::Normal,
        }
    }

    /// Parse a manifest from the §908 YAML-style config:
    ///
    /// ```text
    /// id: planner
    /// version: 1.2.0
    /// description: Planning Agent
    /// capabilities:
    /// - planning
    /// - decomposition
    /// memory: [working, graph]
    /// runtime:
    /// - llama.cpp
    /// permissions:
    /// - graph.read
    /// priority: high
    /// ```
    ///
    /// Scalars are `key: value`; lists are either a `key:` header followed by
    /// `- item` lines or an inline `key: [a, b]`. Unknown keys are ignored;
    /// `id` is required. Dependency-free (no YAML crate).
    pub fn from_manifest(text: &str) -> Result<Self> {
        let mut id = None;
        let mut version = "0.1.0".to_string();
        let mut description = String::new();
        let mut capabilities = Vec::new();
        let mut memory = Vec::new();
        let mut runtimes = Vec::new();
        let mut permissions = Vec::new();
        let mut priority = Priority::Normal;
        let mut current_list: Option<&str> = None;

        for raw in text.lines() {
            let line = raw.trim_end();
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            // List item under the active list key.
            if let Some(item) = line.trim_start().strip_prefix("- ") {
                if let Some(key) = current_list {
                    push_list_item(
                        key,
                        item.trim(),
                        &mut capabilities,
                        &mut memory,
                        &mut runtimes,
                        &mut permissions,
                    );
                }
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            current_list = None;
            match key {
                "id" => id = Some(value.to_string()),
                "version" => version = value.to_string(),
                "description" => description = value.to_string(),
                "priority" => priority = parse_priority(value),
                "capabilities" | "memory" | "runtime" | "runtimes" | "permissions" => {
                    let target = if key == "runtime" { "runtimes" } else { key };
                    if value.is_empty() {
                        current_list = Some(target); // items follow on `- ` lines
                    } else if let Some(inline) =
                        value.strip_prefix('[').and_then(|v| v.strip_suffix(']'))
                    {
                        for item in inline.split(',') {
                            let item = item.trim();
                            if !item.is_empty() {
                                push_list_item(
                                    target,
                                    item,
                                    &mut capabilities,
                                    &mut memory,
                                    &mut runtimes,
                                    &mut permissions,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let id = id.ok_or_else(|| KernelError::other("manifest missing required `id`"))?;
        Ok(AgentManifest {
            id,
            version,
            description,
            capabilities,
            memory,
            runtimes,
            permissions,
            priority,
        })
    }
}

fn parse_priority(value: &str) -> Priority {
    match value.to_ascii_lowercase().as_str() {
        "low" => Priority::Low,
        "high" => Priority::High,
        "critical" => Priority::Critical,
        _ => Priority::Normal,
    }
}

#[allow(clippy::ptr_arg)]
fn push_list_item(
    key: &str,
    item: &str,
    capabilities: &mut Vec<Capability>,
    memory: &mut Vec<String>,
    runtimes: &mut Vec<String>,
    permissions: &mut Vec<String>,
) {
    match key {
        "capabilities" => {
            capabilities.push(item.parse().unwrap_or(Capability::Custom(item.into())))
        }
        "memory" => memory.push(item.to_string()),
        "runtimes" => runtimes.push(item.to_string()),
        "permissions" => permissions.push(item.to_string()),
        _ => {}
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
    fn parses_the_spec_manifest() {
        let text = "\
id: planner
version: 1.2.0
description: Planning Agent
capabilities:
- planning
- decomposition
memory:
- working
- graph
runtime:
- photon
- llama.cpp
permissions:
- graph.read
- graph.write
priority: high";
        let m = AgentManifest::from_manifest(text).unwrap();
        assert_eq!(m.id, "planner");
        assert_eq!(m.version, "1.2.0");
        assert_eq!(m.description, "Planning Agent");
        assert_eq!(m.capabilities.len(), 2);
        assert_eq!(m.capabilities[0], Capability::Planning);
        assert_eq!(
            m.capabilities[1],
            Capability::Custom("decomposition".into())
        );
        assert_eq!(m.memory, vec!["working", "graph"]);
        assert_eq!(m.runtimes, vec!["photon", "llama.cpp"]);
        assert_eq!(m.permissions, vec!["graph.read", "graph.write"]);
        assert_eq!(m.priority, Priority::High);
    }

    #[test]
    fn parses_inline_lists_and_requires_id() {
        let m = AgentManifest::from_manifest("id: x\ncapabilities: [coding, vision]").unwrap();
        assert_eq!(m.capabilities, vec![Capability::Coding, Capability::Vision]);
        assert!(AgentManifest::from_manifest("version: 1.0").is_err());
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
