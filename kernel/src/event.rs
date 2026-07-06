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
    /// A task entered the kernel in the `Created` state (§893).
    TaskCreated(TaskId),
    /// A task began executing on a runtime.
    TaskStarted(TaskId),
    /// A task reached the terminal `Completed` state.
    TaskCompleted(TaskId),
    /// A task failed during execution or verification.
    TaskFailed {
        /// The failing task.
        task: TaskId,
        /// Human-readable failure cause.
        reason: String,
    },
    /// A runtime was loaded and registered with the kernel.
    RuntimeLoaded(RuntimeId),
    /// A memory entry was written or changed.
    MemoryUpdated {
        /// Key of the updated entry.
        key: String,
    },
    /// A knowledge-graph node was added or modified.
    GraphChanged(NodeId),
    /// A plugin was installed into the host.
    PluginInstalled {
        /// Name of the installed plugin.
        name: String,
    },
    /// A policy check denied an action (§929).
    PolicyViolation {
        /// The principal that attempted the action.
        subject: String,
        /// The permission token that was denied.
        action: String,
    },
    /// An agent registered with the capability registry (§910).
    AgentRegistered(AgentId),
    /// A workflow ran all of its tasks to completion (§895).
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
///
/// The subscriber list is bounded only by caller discipline: subscribers that
/// never [`unsubscribe`](EventBus::unsubscribe) accumulate for the process
/// lifetime, so long-lived hosts registering per-request callbacks must
/// unsubscribe them.
#[derive(Default, Clone)]
pub struct InMemoryEventBus {
    inner: Arc<Mutex<Vec<(usize, Subscriber)>>>,
    next_id: Arc<Mutex<usize>>,
}

/// Take a lock, recovering from poisoning: the subscriber list and id counter
/// have no partial-update invariants, and a poisoned bus must degrade rather
/// than turn every later publish into a panic cascade (§894).
fn lock_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
            let guard = lock_recover(&self.inner);
            guard.iter().map(|(_, s)| Arc::clone(s)).collect()
        };
        for sub in subs {
            sub(&event);
        }
    }

    fn subscribe(&self, subscriber: Subscriber) -> usize {
        let mut id_guard = lock_recover(&self.next_id);
        let id = *id_guard;
        *id_guard += 1;
        drop(id_guard);
        lock_recover(&self.inner).push((id, subscriber));
        id
    }

    fn unsubscribe(&self, id: usize) {
        lock_recover(&self.inner).retain(|(sid, _)| *sid != id);
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
