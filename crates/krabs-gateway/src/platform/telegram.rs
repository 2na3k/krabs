//! Telegram adapter.
//!
//! Uses teloxide long-polling. Each incoming text message sends a "…" stub,
//! then streams LLM deltas back by editing that stub in place, throttled to
//! one edit per 300 ms or every 150 new characters (whichever comes first).
//! Responses longer than 4096 characters are split into additional messages.
//!
//! Required env vars:
//!   TELEGRAM_BOT_TOKEN   — BotFather token

use super::{ConversationId, MessageHandler, ResponseStream};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use teloxide::{
    prelude::*,
    types::{ChatAction, MessageId},
};
use tracing::{error, warn};

const EDIT_INTERVAL: Duration = Duration::from_millis(300);
const EDIT_CHAR_THRESHOLD: usize = 150;
const TG_MAX_LEN: usize = 4096;

// ── Config ────────────────────────────────────────────────────────────────────

pub struct TelegramConfig {
    pub bot_token: String,
}

impl TelegramConfig {
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("TELEGRAM_BOT_TOKEN")
            .map_err(|_| anyhow::anyhow!("missing TELEGRAM_BOT_TOKEN"))?;
        anyhow::ensure!(!token.is_empty(), "TELEGRAM_BOT_TOKEN is empty");
        Ok(Self { bot_token: token })
    }
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct TelegramAdapter {
    config: TelegramConfig,
}

impl TelegramAdapter {
    pub fn new(config: TelegramConfig) -> Self {
        Self { config }
    }

    pub async fn start(
        self: Arc<Self>,
        handler: Arc<dyn MessageHandler>,
    ) -> tokio::task::JoinHandle<()> {
        let bot = Bot::new(&self.config.bot_token);

        tokio::spawn(async move {
            let handler_ref = Arc::clone(&handler);

            let dispatch_handler = Update::filter_message().endpoint(
                move |bot: Bot, msg: Message| {
                    let handler = Arc::clone(&handler_ref);
                    async move { handle_message(bot, msg, handler).await }
                },
            );

            Dispatcher::builder(bot, dispatch_handler)
                .enable_ctrlc_handler()
                .build()
                .dispatch()
                .await;
        })
    }
}

// ── Message handler ───────────────────────────────────────────────────────────

async fn handle_message(
    bot: Bot,
    msg: Message,
    handler: Arc<dyn MessageHandler>,
) -> ResponseResult<()> {
    let text = match msg.text() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return respond(()),
    };

    let chat_id = msg.chat.id;
    let conv = ConversationId::new("telegram", chat_id.to_string());

    // Show typing indicator (best-effort)
    let _ = bot.send_chat_action(chat_id, ChatAction::Typing).await;

    // Send the "…" stub that we'll edit in place as the response streams in
    let stub = match bot.send_message(chat_id, "…").await {
        Ok(m) => m,
        Err(e) => {
            error!("Telegram: failed to send stub: {}", e);
            return respond(());
        }
    };

    let response = Box::new(TelegramResponseStream::new(
        bot.clone(),
        chat_id,
        stub.id,
    ));

    // Spawn so we return to teloxide immediately while the agent runs
    tokio::spawn(async move {
        if let Err(e) = handler.on_message(conv, text, response).await {
            error!("Telegram: on_message error: {}", e);
        }
    });

    respond(())
}

// ── ResponseStream ────────────────────────────────────────────────────────────

pub struct TelegramResponseStream {
    bot: Bot,
    chat_id: ChatId,
    message_id: MessageId,
    last_edit: Instant,
    last_edit_len: usize,
}

impl TelegramResponseStream {
    pub fn new(bot: Bot, chat_id: ChatId, message_id: MessageId) -> Self {
        Self {
            bot,
            chat_id,
            message_id,
            last_edit: Instant::now()
                .checked_sub(EDIT_INTERVAL)
                .unwrap_or_else(Instant::now),
            last_edit_len: 0,
        }
    }

    async fn do_edit(&mut self, text: &str) {
        let truncated = truncate_to_limit(text, TG_MAX_LEN);
        match self
            .bot
            .edit_message_text(self.chat_id, self.message_id, truncated)
            .await
        {
            Ok(_) => {
                self.last_edit = Instant::now();
                self.last_edit_len = text.len();
            }
            Err(e) => {
                // "message is not modified" is harmless — skip. Log others.
                let msg = e.to_string();
                if !msg.contains("message is not modified") {
                    warn!("Telegram: edit_message_text error: {}", e);
                }
            }
        }
    }
}

#[async_trait]
impl ResponseStream for TelegramResponseStream {
    /// Throttled live edit: fires when 300 ms have passed OR 150 new chars
    /// have accumulated since the last edit.
    async fn update(&mut self, accumulated: &str) -> Result<()> {
        let elapsed = self.last_edit.elapsed();
        let new_chars = accumulated.len().saturating_sub(self.last_edit_len);

        if elapsed >= EDIT_INTERVAL || new_chars >= EDIT_CHAR_THRESHOLD {
            self.do_edit(accumulated).await;
        }
        Ok(())
    }

    /// Show a brief status line while the agent is calling tools.
    async fn set_status(&mut self, status: &str) -> Result<()> {
        let display = format!("⚙ {}…", status);
        self.do_edit(&display).await;
        // Reset len so next delta triggers a real update
        self.last_edit_len = 0;
        Ok(())
    }

    /// Always flush the final text. If it exceeds 4096 chars, the first chunk
    /// edits the stub and overflow is sent as additional messages.
    async fn finalize(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            self.do_edit("…").await;
            return Ok(());
        }

        let chunks = chunk_text(text, TG_MAX_LEN);

        // Edit stub in place with the first chunk
        self.do_edit(&chunks[0]).await;

        // Send overflow as follow-up messages
        for chunk in &chunks[1..] {
            if let Err(e) = self.bot.send_message(self.chat_id, chunk).await {
                warn!("Telegram: failed to send overflow chunk: {}", e);
            }
        }

        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Split text into chunks of at most `max_chars` characters (on char boundary).
fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(max_chars)
        .map(|c| c.iter().collect())
        .collect()
}

/// Truncate to `max_chars` with a trailing "…" if cut.
fn truncate_to_limit(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max_chars - 1).collect();
    s.push('…');
    s
}
