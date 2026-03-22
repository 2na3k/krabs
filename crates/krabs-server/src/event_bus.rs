use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, RwLock};

/// A single event published to the bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SseEvent {
    pub id: u64,
    pub event_type: String,
    pub data: String,
}

/// Per-agent event bus for SSE streaming.
///
/// Supports both live subscription (via `broadcast`) and reconnection (via a
/// fixed-capacity circular replay buffer). Events are assigned monotonically
/// increasing IDs so clients can request replay from a specific point.
pub struct SessionEventBus {
    tx: broadcast::Sender<SseEvent>,
    buffer: RwLock<VecDeque<SseEvent>>,
    capacity: usize,
    next_id: AtomicU64,
}

impl SessionEventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Self {
            tx,
            buffer: RwLock::new(VecDeque::with_capacity(capacity)),
            capacity,
            next_id: AtomicU64::new(1),
        }
    }

    /// Publish an event to all live subscribers and the replay buffer.
    pub async fn publish(&self, event_type: &str, data: String) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let event = SseEvent {
            id,
            event_type: event_type.to_string(),
            data,
        };

        // Write to replay buffer first (under write lock).
        {
            let mut buf = self.buffer.write().await;
            if buf.len() >= self.capacity {
                buf.pop_front();
            }
            buf.push_back(event.clone());
        }

        // Broadcast to live receivers. Lagged receivers are silently dropped.
        let _ = self.tx.send(event);
        id
    }

    /// Subscribe: returns (replay events after `last_id`, live receiver).
    ///
    /// The broadcast receiver is created *before* snapshotting the buffer to
    /// prevent a race where an event is published between the snapshot and
    /// the subscription.
    pub async fn subscribe(
        &self,
        last_id: Option<u64>,
    ) -> (Vec<SseEvent>, broadcast::Receiver<SseEvent>) {
        // Subscribe first to prevent the gap.
        let rx = self.tx.subscribe();

        let replay = match last_id {
            Some(id) => {
                let buf = self.buffer.read().await;
                buf.iter().filter(|e| e.id > id).cloned().collect()
            }
            None => Vec::new(),
        };

        (replay, rx)
    }
}
