//! Slack adapter — not yet implemented.
//!
//! Will use the Slack Events API (webhook) + `chat.postMessage` /
//! `chat.update` to stream deltas into a reply in the originating thread.
//!
//! Required env vars (when implemented):
//!   SLACK_BOT_TOKEN
//!   SLACK_SIGNING_SECRET
//!   SLACK_PORT  (default: 3002)

use super::{ConversationId, MessageHandler, ResponseStream};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

pub struct SlackConfig {
    pub bot_token: String,
    pub signing_secret: String,
    pub port: u16,
}

impl SlackConfig {
    pub fn from_env() -> Result<Self> {
        let bot_token = std::env::var("SLACK_BOT_TOKEN")
            .map_err(|_| anyhow::anyhow!("missing SLACK_BOT_TOKEN"))?;
        anyhow::ensure!(!bot_token.is_empty(), "SLACK_BOT_TOKEN is empty");
        let signing_secret = std::env::var("SLACK_SIGNING_SECRET")
            .map_err(|_| anyhow::anyhow!("missing SLACK_SIGNING_SECRET"))?;
        anyhow::ensure!(!signing_secret.is_empty(), "SLACK_SIGNING_SECRET is empty");
        Ok(Self {
            bot_token,
            signing_secret,
            port: std::env::var("SLACK_PORT")
                .unwrap_or_else(|_| "3002".into())
                .parse()?,
        })
    }
}

pub struct SlackAdapter {
    #[allow(dead_code)]
    config: SlackConfig,
}

impl SlackAdapter {
    pub fn new(config: SlackConfig) -> Self {
        Self { config }
    }

    pub async fn start(
        self: Arc<Self>,
        _handler: Arc<dyn MessageHandler>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // TODO: boot axum webhook server, verify X-Slack-Signature,
            // parse app_mention / message events, map to handler.on_message,
            // implement SlackResponseStream with chat.update throttling.
            tracing::warn!("Slack adapter is not yet implemented");
        })
    }
}

/// Streams deltas by calling `chat.update` on a posted message stub.
pub struct SlackResponseStream {
    // TODO: bot token, channel, thread_ts, message_ts
}

#[async_trait]
impl ResponseStream for SlackResponseStream {
    async fn update(&mut self, _accumulated: &str) -> Result<()> {
        // TODO: throttled chat.update
        Ok(())
    }

    async fn set_status(&mut self, _status: &str) -> Result<()> {
        // TODO: chat.update with status line
        Ok(())
    }

    async fn finalize(&mut self, _text: &str) -> Result<()> {
        // TODO: final chat.update
        Ok(())
    }
}

/// Unused — makes ConversationId available in this module.
#[allow(dead_code)]
fn _conv(_: ConversationId) {}
