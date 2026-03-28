use crate::client::{GatewayEvent, KrabsServerClient};
use crate::platform::{ConversationId, MessageHandler, ResponseStream};
use crate::queue::{ConvWorker, EnqueueResult, QueuedMessage, RunnerFn};
use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use futures_util::StreamExt;
use std::sync::{Arc, OnceLock, Weak};
use tracing::{error, info, warn};

const QUEUE_CAPACITY: usize = 10;

pub struct Gateway {
    client: KrabsServerClient,
    /// conv_id.key() → krabs-server agent_id
    sessions: DashMap<String, String>,
    /// conv_id.key() → per-conversation sequential worker
    workers: DashMap<String, ConvWorker>,
    /// Weak self-reference so workers can call back into execute_turn.
    self_ref: OnceLock<Weak<Gateway>>,
}

impl Gateway {
    pub fn new(client: KrabsServerClient) -> Arc<Self> {
        let gw = Arc::new(Self {
            client,
            sessions: DashMap::new(),
            workers: DashMap::new(),
            self_ref: OnceLock::new(),
        });
        // Store weak reference so workers can call back without a reference cycle.
        let _ = gw.self_ref.set(Arc::downgrade(&gw));
        gw
    }

    async fn get_or_create_agent(&self, conv: &ConversationId) -> Result<String> {
        let key = conv.key();
        if let Some(id) = self.sessions.get(&key) {
            return Ok(id.clone());
        }
        let agent_id = self.client.create_agent(Some(&key)).await?;
        info!("created agent {} for conv {}", agent_id, key);
        self.sessions.insert(key, agent_id.clone());
        Ok(agent_id)
    }

    /// The actual LLM turn — called sequentially by the ConvWorker.
    async fn execute_turn(
        &self,
        conv: ConversationId,
        text: String,
        mut response: Box<dyn ResponseStream>,
    ) -> Result<()> {
        let agent_id = self.get_or_create_agent(&conv).await?;

        let mut stream = match self.client.chat(&agent_id, &text).await {
            Ok(s) => s,
            Err(e) => {
                error!("chat call failed for agent {}: {}", agent_id, e);
                let _ = response.finalize("Sorry, something went wrong. Please try again.").await;
                return Err(e);
            }
        };

        let mut buf = String::new();

        while let Some(event) = stream.next().await {
            match event {
                Ok(GatewayEvent::Delta { text: delta }) => {
                    buf.push_str(&delta);
                    if let Err(e) = response.update(&buf).await {
                        warn!("response.update error: {}", e);
                    }
                }
                Ok(GatewayEvent::Status { text: status }) => {
                    if let Err(e) = response.set_status(&status).await {
                        warn!("response.set_status error: {}", e);
                    }
                }
                Ok(GatewayEvent::Done) => break,
                Err(e) => {
                    error!("stream error for agent {}: {}", agent_id, e);
                    break;
                }
            }
        }

        if let Err(e) = response.finalize(&buf).await {
            warn!("response.finalize error: {}", e);
        }
        Ok(())
    }

    fn get_or_create_worker(&self, conv: &ConversationId) -> ConvWorker {
        let key = conv.key();

        // Atomically get-or-create; also replace dead workers.
        let existing = self.workers.entry(key.clone()).or_insert_with(|| {
            self.make_worker(conv)
        });

        // If the worker task has exited (e.g. after a panic), replace it.
        if existing.is_dead() {
            drop(existing);
            let fresh = self.make_worker(conv);
            self.workers.insert(key, fresh.clone());
            fresh
        } else {
            existing.clone()
        }
    }

    fn make_worker(&self, conv: &ConversationId) -> ConvWorker {
        let weak = self
            .self_ref
            .get()
            .expect("Gateway self_ref not initialised")
            .clone();
        let conv_clone = conv.clone();
        let key = conv.key();

        let runner: RunnerFn = Arc::new(move |text, response| {
            let weak = weak.clone();
            let conv = conv_clone.clone();
            Box::pin(async move {
                match weak.upgrade() {
                    Some(gw) => gw.execute_turn(conv, text, response).await,
                    None => {
                        warn!("Gateway dropped — discarding queued turn");
                        Ok(())
                    }
                }
            })
        });

        ConvWorker::new(QUEUE_CAPACITY, runner, key)
    }
}

#[async_trait]
impl MessageHandler for Gateway {
    async fn on_message(
        &self,
        conv: ConversationId,
        text: String,
        response: Box<dyn ResponseStream>,
    ) -> Result<()> {
        let worker = self.get_or_create_worker(&conv);
        let key = conv.key();

        match worker.try_enqueue(QueuedMessage { text, response }) {
            EnqueueResult::Enqueued => {
                info!("conv {}: message enqueued", key);
                Ok(())
            }
            EnqueueResult::Full(mut rejected) => {
                warn!("conv {}: queue full, dropping message", key);
                let _ = rejected
                    .finalize("I'm receiving too many messages. Please wait a moment and try again.")
                    .await;
                Ok(())
            }
        }
    }
}
