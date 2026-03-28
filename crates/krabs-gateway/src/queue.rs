use crate::platform::ResponseStream;
use std::pin::Pin;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub type TurnFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
pub type RunnerFn =
    Arc<dyn Fn(String, Box<dyn ResponseStream>) -> TurnFuture + Send + Sync>;

pub struct QueuedMessage {
    pub text: String,
    pub response: Box<dyn ResponseStream>,
}

pub enum EnqueueResult {
    Enqueued,
    /// Queue was full — returns the response so the caller can finalize it.
    Full(Box<dyn ResponseStream>),
}

/// Per-conversation worker: drains messages sequentially so an agent is never
/// hit with two concurrent turns for the same conversation.
#[derive(Clone)]
pub struct ConvWorker {
    tx: mpsc::Sender<QueuedMessage>,
}

impl ConvWorker {
    pub fn new(capacity: usize, runner: RunnerFn, conv_key: String) -> Self {
        let (tx, mut rx) = mpsc::channel::<QueuedMessage>(capacity);

        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                info!("conv {}: processing queued message", conv_key);
                if let Err(e) = runner(msg.text, msg.response).await {
                    warn!("conv {}: turn error: {}", conv_key, e);
                }
            }
            info!("conv {}: worker exiting (channel closed)", conv_key);
        });

        Self { tx }
    }

    pub fn try_enqueue(&self, msg: QueuedMessage) -> EnqueueResult {
        match self.tx.try_send(msg) {
            Ok(()) => EnqueueResult::Enqueued,
            Err(mpsc::error::TrySendError::Full(m)) => EnqueueResult::Full(m.response),
            Err(mpsc::error::TrySendError::Closed(m)) => {
                // Worker exited — treat as full; caller will recreate on next message
                EnqueueResult::Full(m.response)
            }
        }
    }

    /// True if the underlying worker task has exited (channel closed).
    pub fn is_dead(&self) -> bool {
        self.tx.is_closed()
    }
}
