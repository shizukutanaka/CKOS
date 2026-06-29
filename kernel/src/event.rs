//! Event bus (§894) — the loosely-coupled backbone for inter-module
//! communication.
//!
//! Modules publish [`Event`]s and subscribe with callbacks. The bus is
//! synchronous and in-process here; the [`EventBus`] trait lets a distributed
//! transport (NATS, Kafka, the Service Mesh of §916) be substituted later
//! without touching publishers.

use crate::id::{AgentId, NodeId, RuntimeId, TaskId, WorkflowId};
use std::sync::{Arc, Mutex};

/// Events emitted across the kernel (representative set from §894 + §923).
#[derive(Debug, Clone)]
pub enum Event {
    TaskCreated(TaskId),
    TaskStarted(TaskId),
    TaskCompleted(TaskId),
    TaskFailed { task: TaskId, reason: String },
    RuntimeLoaded(RuntimeId),
    MemoryUpdated { key: String },
    GraphChanged(NodeId),
    PluginInstalled { name: String },
    PolicyViolation { subject: String, action: String },
    AgentRegistered(AgentId),
    WorkflowCompleted(WorkflowId),
}

impl Event {
    /// Short topic label, useful for filtering and metrics (§933).
    pub fn topic(&self) -> &'static str {
        match self {
            Event::TaskCreated(_) => "task.created",
            Event::TaskStarted(_) => "task.started",
            Event::TaskCompleted(_) => "task.completed",
            Event::TaskFailed { .. } => "task.failed",
            Event::RuntimeLoaded(_) => "runtime.loaded",
            Event::MemoryUpdated { .. } => "memory.updated",
            Event::GraphChanged(_) => "graph.changed",
            Event::PluginInstalled { .. } => "plugin.installed",
            Event::PolicyViolation { .. } => "policy.violation",
            Event::AgentRegistered(_) => "agent.registered",
            Event::WorkflowCompleted(_) => "workflow.completed",
        }
    }
}

/// A subscriber callback. `Send + Sync` so the bus can be shared across threads.
pub type Subscriber = Arc<dyn Fn(&Event) + Send + Sync>;

/// Abstraction over an event transport so the in-process bus can be swapped for
/// a networked one (§916) without changing callers.
pub trait EventBus: Send + Sync {
    /// Publish an event to all interested subscribers.
    fn publish(&self, event: Event);
    /// Subscribe to every event. Returns a subscription id for later removal.
    fn subscribe(&self, subscriber: Subscriber) -> usize;
    /// Remove a previously registered subscription.
    fn unsubscribe(&self, id: usize);
}

/// Simple synchronous, in-memory event bus.
#[derive(Default, Clone)]
pub struct InMemoryEventBus {
    inner: Arc<Mutex<Vec<(usize, Subscriber)>>>,
    next_id: Arc<Mutex<usize>>,
}

impl InMemoryEventBus {
    /// Create an empty bus.
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventBus for InMemoryEventBus {
    fn publish(&self, event: Event) {
        // Clone the subscriber list so callbacks can't deadlock by (un)subscribing.
        let subs: Vec<Subscriber> = {
            let guard = self.inner.lock().expect("event bus poisoned");
            guard.iter().map(|(_, s)| Arc::clone(s)).collect()
        };
        for sub in subs {
            sub(&event);
        }
    }

    fn subscribe(&self, subscriber: Subscriber) -> usize {
        let mut id_guard = self.next_id.lock().expect("event bus poisoned");
        let id = *id_guard;
        *id_guard += 1;
        drop(id_guard);
        self.inner
            .lock()
            .expect("event bus poisoned")
            .push((id, subscriber));
        id
    }

    fn unsubscribe(&self, id: usize) {
        self.inner
            .lock()
            .expect("event bus poisoned")
            .retain(|(sid, _)| *sid != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn delivers_to_subscribers() {
        let bus = InMemoryEventBus::new();
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let id = bus.subscribe(Arc::new(move |_e: &Event| {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        bus.publish(Event::TaskCreated(TaskId::new()));
        bus.publish(Event::TaskCreated(TaskId::new()));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        bus.unsubscribe(id);
        bus.publish(Event::TaskCreated(TaskId::new()));
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
