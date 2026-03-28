use anyhow::Result;
use async_trait::async_trait;

/// Identifies a conversation across any platform.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ConversationId {
    pub platform: String,
    /// Platform-native conversation key (chat_id, channel+thread, phone number, …).
    pub id: String,
}

impl ConversationId {
    pub fn new(platform: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            platform: platform.into(),
            id: id.into(),
        }
    }

    /// Stable string key for use in maps.
    pub fn key(&self) -> String {
        format!("{}:{}", self.platform, self.id)
    }
}

/// Platform-specific handle for streaming a response back to one conversation.
///
/// The gateway drives this with accumulated text; platforms decide how to
/// deliver it (edit in place, append, buffer-and-send, etc.).
#[async_trait]
pub trait ResponseStream: Send + Sync {
    /// Called repeatedly with the full accumulated text so far.
    /// Platforms that support live editing (Telegram, Slack) throttle and edit.
    /// Platforms that don't (WhatsApp) ignore until `finalize`.
    async fn update(&mut self, accumulated: &str) -> Result<()>;

    /// A short status line from the agent (e.g. "⚙ running bash…").
    async fn set_status(&mut self, status: &str) -> Result<()>;

    /// The agent is done. `text` is the complete final response.
    async fn finalize(&mut self, text: &str) -> Result<()>;
}

/// Called by platform adapters when they receive an inbound message.
#[async_trait]
pub trait MessageHandler: Send + Sync {
    async fn on_message(
        &self,
        conv: ConversationId,
        text: String,
        response: Box<dyn ResponseStream>,
    ) -> Result<()>;
}

pub mod slack;
pub mod telegram;
pub mod whatsapp;
