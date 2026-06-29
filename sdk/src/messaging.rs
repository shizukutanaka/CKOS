//! Agent messaging (§914–§916).
//!
//! Agents communicate by passing [`Message`]s (§915) over a [`MessageBus`]
//! (§914): `Planner → Bus → Reasoner → Verifier → Output`. The [`ServiceMesh`]
//! (§916) wraps the bus with the cross-cutting concerns the spec lists —
//! capability-based routing, round-robin load balancing across providers, and
//! delivery accounting — so agents address *a capability*, not a specific peer.

use ckos_kernel::capability::Capability;
use ckos_kernel::error::{KernelError, Result};
use ckos_kernel::task::Priority;
use ckos_kernel::{AgentId, DocumentId, NodeId};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

static MSG_SEQ: AtomicU64 = AtomicU64::new(1);

/// The payload of a message (§915): references into the graph/memory plus a body.
#[derive(Debug, Clone, Default)]
pub struct Payload {
    /// Optional graph node the message refers to.
    pub graph_id: Option<NodeId>,
    /// Optional memory document the message refers to.
    pub memory_ref: Option<DocumentId>,
    /// Free-form body.
    pub body: String,
}

/// An inter-agent message (§915).
#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub source: AgentId,
    pub destination: AgentId,
    pub msg_type: String,
    pub priority: Priority,
    pub payload: Payload,
}

impl Message {
    /// Create a message between two agents.
    pub fn new(source: AgentId, destination: AgentId, msg_type: impl Into<String>) -> Self {
        Message {
            id: format!("msg-{:x}", MSG_SEQ.fetch_add(1, Ordering::Relaxed)),
            source,
            destination,
            msg_type: msg_type.into(),
            priority: Priority::Normal,
            payload: Payload::default(),
        }
    }

    /// Builder: set priority.
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Builder: set the body.
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.payload.body = body.into();
        self
    }

    /// Builder: attach a graph reference.
    pub fn with_graph(mut self, id: NodeId) -> Self {
        self.payload.graph_id = Some(id);
        self
    }

    /// Builder: attach a memory reference.
    pub fn with_memory(mut self, id: DocumentId) -> Self {
        self.payload.memory_ref = Some(id);
        self
    }
}

/// A loosely-coupled, in-process message bus with per-agent inboxes (§914).
#[derive(Default)]
pub struct MessageBus {
    inboxes: HashMap<AgentId, VecDeque<Message>>,
}

impl MessageBus {
    /// Create an empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensure an inbox exists for an agent.
    pub fn register(&mut self, agent: &AgentId) {
        self.inboxes.entry(agent.clone()).or_default();
    }

    /// Whether an agent has a registered inbox.
    pub fn is_registered(&self, agent: &AgentId) -> bool {
        self.inboxes.contains_key(agent)
    }

    /// Deliver a message to its destination inbox. Fails if the destination has
    /// no inbox (an undelivered message is never silently dropped).
    pub fn send(&mut self, message: Message) -> Result<()> {
        match self.inboxes.get_mut(&message.destination) {
            Some(inbox) => {
                inbox.push_back(message);
                Ok(())
            }
            None => Err(KernelError::NotFound(format!(
                "inbox for {}",
                message.destination
            ))),
        }
    }

    /// Drain an agent's inbox, highest priority first (stable within a priority).
    pub fn receive(&mut self, agent: &AgentId) -> Vec<Message> {
        let Some(inbox) = self.inboxes.get_mut(agent) else {
            return Vec::new();
        };
        let mut msgs: Vec<Message> = inbox.drain(..).collect();
        // Higher priority first; sort is stable so same-priority keeps FIFO order.
        msgs.sort_by(|a, b| b.priority.cmp(&a.priority));
        msgs
    }

    /// Number of messages waiting in an agent's inbox.
    pub fn pending(&self, agent: &AgentId) -> usize {
        self.inboxes.get(agent).map_or(0, |i| i.len())
    }
}

/// Service mesh over a [`MessageBus`] (§916): capability routing, round-robin
/// load balancing, and delivery accounting.
#[derive(Default)]
pub struct ServiceMesh {
    bus: MessageBus,
    /// Capability token → providers offering it.
    providers: HashMap<String, Vec<AgentId>>,
    /// Round-robin cursor per capability.
    cursor: HashMap<String, usize>,
    /// Count of successfully delivered messages.
    delivered: usize,
}

impl ServiceMesh {
    /// Create an empty mesh.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent as a provider of a capability (also creates its inbox).
    pub fn register_provider(&mut self, agent: &AgentId, capability: &Capability) {
        self.bus.register(agent);
        self.providers
            .entry(capability.as_token().to_string())
            .or_default()
            .push(agent.clone());
    }

    /// Borrow the underlying bus (e.g. to receive messages).
    pub fn bus_mut(&mut self) -> &mut MessageBus {
        &mut self.bus
    }

    /// Total messages successfully delivered through the mesh.
    pub fn delivered(&self) -> usize {
        self.delivered
    }

    /// Route a message to *some* provider of `capability`, chosen round-robin
    /// for load balancing (§916). Returns the chosen agent, or an error if no
    /// provider is registered.
    pub fn route(
        &mut self,
        source: AgentId,
        capability: &Capability,
        msg_type: impl Into<String>,
        payload: Payload,
    ) -> Result<AgentId> {
        let token = capability.as_token().to_string();
        let providers = self
            .providers
            .get(&token)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| KernelError::CapabilityUnavailable(token.clone()))?;

        let cursor = self.cursor.entry(token).or_insert(0);
        let chosen = providers[*cursor % providers.len()].clone();
        *cursor = cursor.wrapping_add(1);

        let mut message = Message::new(source, chosen.clone(), msg_type);
        message.payload = payload;
        self.bus.send(message)?;
        self.delivered += 1;
        Ok(chosen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_in_priority_order() {
        let mut bus = MessageBus::new();
        let planner = AgentId::new();
        let verifier = AgentId::new();
        bus.register(&verifier);

        bus.send(Message::new(planner.clone(), verifier.clone(), "low"))
            .unwrap();
        bus.send(
            Message::new(planner.clone(), verifier.clone(), "urgent")
                .with_priority(Priority::Critical),
        )
        .unwrap();

        let received = bus.receive(&verifier);
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].msg_type, "urgent"); // critical first
        assert!(bus.pending(&verifier) == 0); // drained
    }

    #[test]
    fn send_to_unregistered_fails() {
        let mut bus = MessageBus::new();
        let a = AgentId::new();
        let b = AgentId::new();
        assert!(matches!(
            bus.send(Message::new(a, b, "x")),
            Err(KernelError::NotFound(_))
        ));
    }

    #[test]
    fn mesh_load_balances_round_robin() {
        let mut mesh = ServiceMesh::new();
        let r1 = AgentId::new();
        let r2 = AgentId::new();
        mesh.register_provider(&r1, &Capability::Reasoning);
        mesh.register_provider(&r2, &Capability::Reasoning);
        let source = AgentId::new();

        let a = mesh
            .route(
                source.clone(),
                &Capability::Reasoning,
                "t",
                Payload::default(),
            )
            .unwrap();
        let b = mesh
            .route(
                source.clone(),
                &Capability::Reasoning,
                "t",
                Payload::default(),
            )
            .unwrap();
        let c = mesh
            .route(
                source.clone(),
                &Capability::Reasoning,
                "t",
                Payload::default(),
            )
            .unwrap();
        // Alternates between the two providers.
        assert_ne!(a, b);
        assert_eq!(a, c);
        assert_eq!(mesh.delivered(), 3);
        // r1 and r2 each received messages.
        assert_eq!(
            mesh.bus_mut().receive(&r1).len() + mesh.bus_mut().receive(&r2).len(),
            3
        );
    }

    #[test]
    fn mesh_route_without_provider_errors() {
        let mut mesh = ServiceMesh::new();
        let source = AgentId::new();
        assert!(matches!(
            mesh.route(source, &Capability::Vision, "t", Payload::default()),
            Err(KernelError::CapabilityUnavailable(_))
        ));
    }
}
