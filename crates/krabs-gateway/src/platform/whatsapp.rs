//! WhatsApp Cloud API adapter.
//!
//! Incoming: webhook (GET verify + POST messages) via axum.
//! Outgoing: POST to graph.facebook.com — no message editing, so the response
//!           is buffered in the gateway and sent as one (or more) messages on
//!           `finalize`.
//!
//! Required env vars:
//!   WHATSAPP_TOKEN            — permanent access token (or system user token)
//!   WHATSAPP_PHONE_NUMBER_ID  — numeric phone number ID from Meta developer console
//!   WHATSAPP_APP_SECRET       — app secret for X-Hub-Signature-256 verification
//!   WHATSAPP_VERIFY_TOKEN     — arbitrary string you set in the Meta webhook config
//!   WHATSAPP_PORT             — port for the webhook server (default: 3001)

use super::{ConversationId, MessageHandler, ResponseStream};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
    Router,
};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WhatsAppConfig {
    /// Bearer token for the Cloud API.
    pub token: String,
    /// Numeric phone number ID (from Meta developer console).
    pub phone_number_id: String,
    /// App secret used to verify X-Hub-Signature-256.
    pub app_secret: String,
    /// The verify token you configured in the Meta webhook settings.
    pub verify_token: String,
    /// Port for the webhook HTTP server.
    pub port: u16,
}

impl WhatsAppConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            token: required_env("WHATSAPP_TOKEN")?,
            phone_number_id: required_env("WHATSAPP_PHONE_NUMBER_ID")?,
            app_secret: required_env("WHATSAPP_APP_SECRET")?,
            verify_token: required_env("WHATSAPP_VERIFY_TOKEN")?,
            port: std::env::var("WHATSAPP_PORT")
                .unwrap_or_else(|_| "3001".into())
                .parse()?,
        })
    }
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct WhatsAppAdapter {
    config: Arc<WhatsAppConfig>,
    http: Client,
}

impl WhatsAppAdapter {
    pub fn new(config: WhatsAppConfig) -> Self {
        Self {
            config: Arc::new(config),
            http: Client::new(),
        }
    }

    /// Start the webhook server. Returns a `JoinHandle` — caller awaits it or
    /// races it against other platform tasks.
    pub async fn start(
        self: Arc<Self>,
        handler: Arc<dyn MessageHandler>,
    ) -> tokio::task::JoinHandle<()> {
        let port = self.config.port;
        let state = Arc::new(WebhookState {
            config: Arc::clone(&self.config),
            handler,
            http: self.http.clone(),
        });

        let app = Router::new()
            .route(
                "/webhook/whatsapp",
                get(handle_verify).post(handle_messages),
            )
            .with_state(state);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
        info!("WhatsApp webhook listening on {}", addr);

        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!("WhatsApp: failed to bind {}: {}", addr, e);
                    return;
                }
            };
            if let Err(e) = axum::serve(listener, app).await {
                error!("WhatsApp webhook server error: {}", e);
            }
        })
    }
}

// ── Shared axum state ─────────────────────────────────────────────────────────

struct WebhookState {
    config: Arc<WhatsAppConfig>,
    handler: Arc<dyn MessageHandler>,
    http: Client,
}

// ── Webhook verification (GET) ────────────────────────────────────────────────

#[derive(Deserialize)]
struct VerifyQuery {
    #[serde(rename = "hub.mode")]
    mode: String,
    #[serde(rename = "hub.verify_token")]
    verify_token: String,
    #[serde(rename = "hub.challenge")]
    challenge: String,
}

async fn handle_verify(
    State(state): State<Arc<WebhookState>>,
    Query(q): Query<VerifyQuery>,
) -> Result<String, StatusCode> {
    if q.mode == "subscribe" && q.verify_token == state.config.verify_token {
        info!("WhatsApp webhook verified");
        Ok(q.challenge)
    } else {
        warn!("WhatsApp webhook verify failed: mode={} token={}", q.mode, q.verify_token);
        Err(StatusCode::FORBIDDEN)
    }
}

// ── Incoming messages (POST) ──────────────────────────────────────────────────

async fn handle_messages(
    State(state): State<Arc<WebhookState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    // 1. Verify signature
    if let Some(sig) = headers.get("x-hub-signature-256").and_then(|v| v.to_str().ok()) {
        if !verify_signature(&body, sig, &state.config.app_secret) {
            warn!("WhatsApp: invalid signature, rejecting payload");
            return StatusCode::UNAUTHORIZED;
        }
    } else {
        warn!("WhatsApp: missing X-Hub-Signature-256, rejecting payload");
        return StatusCode::UNAUTHORIZED;
    }

    // 2. Parse payload
    let payload: WebhookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            error!("WhatsApp: failed to parse payload: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    // 3. Extract text messages and dispatch concurrently
    for entry in payload.entry {
        for change in entry.changes {
            if change.field != "messages" {
                continue;
            }
            let value = change.value;

            // Mark messages as read and dispatch
            for msg in value.messages.unwrap_or_default() {
                if msg.message_type != "text" {
                    debug!("WhatsApp: ignoring non-text message type={}", msg.message_type);
                    continue;
                }
                let text = match msg.text.as_ref().map(|t| t.body.clone()) {
                    Some(t) => t,
                    None => continue,
                };

                let conv = ConversationId::new("whatsapp", &msg.from);
                let handler = Arc::clone(&state.handler);
                let http = state.http.clone();
                let config = Arc::clone(&state.config);
                let msg_id = msg.id.clone();
                let from = msg.from.clone();

                tokio::spawn(async move {
                    // Mark as read before responding
                    if let Err(e) = mark_read(&http, &config, &msg_id, &from).await {
                        warn!("WhatsApp: mark_read failed: {}", e);
                    }

                    let response = Box::new(WhatsAppResponseStream::new(
                        from.clone(),
                        Arc::clone(&config),
                        http.clone(),
                    ));

                    if let Err(e) = handler.on_message(conv, text, response).await {
                        error!("WhatsApp: on_message error for {}: {}", from, e);
                    }
                });
            }
        }
    }

    // Meta requires 200 within 5 seconds; all work is spawned above.
    StatusCode::OK
}

// ── ResponseStream ────────────────────────────────────────────────────────────

/// WhatsApp doesn't support message editing, so `update` and `set_status` are
/// no-ops. `finalize` sends the complete text, chunked at 4096 characters.
pub struct WhatsAppResponseStream {
    to: String,
    config: Arc<WhatsAppConfig>,
    http: Client,
}

impl WhatsAppResponseStream {
    pub fn new(to: String, config: Arc<WhatsAppConfig>, http: Client) -> Self {
        Self { to, config, http }
    }
}

#[async_trait]
impl ResponseStream for WhatsAppResponseStream {
    async fn update(&mut self, _accumulated: &str) -> Result<()> {
        // WhatsApp has no message-edit API; nothing to do mid-stream.
        Ok(())
    }

    async fn set_status(&mut self, _status: &str) -> Result<()> {
        // Could send a reaction or a brief status message here in the future.
        Ok(())
    }

    async fn finalize(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        // Chunk into ≤4096-character messages (WhatsApp limit).
        for chunk in chunk_text(text, 4096) {
            send_text_message(&self.http, &self.config, &self.to, &chunk).await?;
        }
        Ok(())
    }
}

// ── Graph API calls ───────────────────────────────────────────────────────────

async fn send_text_message(
    http: &Client,
    config: &WhatsAppConfig,
    to: &str,
    body: &str,
) -> Result<()> {
    let url = format!(
        "https://graph.facebook.com/v20.0/{}/messages",
        config.phone_number_id
    );

    #[derive(Serialize)]
    struct TextBody<'a> {
        body: &'a str,
        preview_url: bool,
    }
    #[derive(Serialize)]
    struct Payload<'a> {
        messaging_product: &'static str,
        recipient_type: &'static str,
        to: &'a str,
        #[serde(rename = "type")]
        message_type: &'static str,
        text: TextBody<'a>,
    }

    let payload = Payload {
        messaging_product: "whatsapp",
        recipient_type: "individual",
        to,
        message_type: "text",
        text: TextBody {
            body,
            preview_url: false,
        },
    };

    let resp = http
        .post(&url)
        .bearer_auth(&config.token)
        .json(&payload)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("WhatsApp send_message failed {}: {}", status, text));
    }

    Ok(())
}

async fn mark_read(
    http: &Client,
    config: &WhatsAppConfig,
    message_id: &str,
    _to: &str,
) -> Result<()> {
    let url = format!(
        "https://graph.facebook.com/v20.0/{}/messages",
        config.phone_number_id
    );

    #[derive(Serialize)]
    struct Payload<'a> {
        messaging_product: &'static str,
        status: &'static str,
        message_id: &'a str,
    }

    let payload = Payload {
        messaging_product: "whatsapp",
        status: "read",
        message_id,
    };

    let resp = http
        .post(&url)
        .bearer_auth(&config.token)
        .json(&payload)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("WhatsApp mark_read failed {}: {}", status, text));
    }

    Ok(())
}

// ── Signature verification ────────────────────────────────────────────────────

fn verify_signature(payload: &[u8], signature_header: &str, app_secret: &str) -> bool {
    type HmacSha256 = Hmac<Sha256>;

    let hex_sig = signature_header
        .strip_prefix("sha256=")
        .unwrap_or(signature_header);

    let sig_bytes = match hex::decode(hex_sig) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let mut mac = match HmacSha256::new_from_slice(app_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(payload);
    mac.verify_slice(&sig_bytes).is_ok()
}

// ── Payload types ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WebhookPayload {
    entry: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    changes: Vec<Change>,
}

#[derive(Deserialize)]
struct Change {
    field: String,
    value: ChangeValue,
}

#[derive(Deserialize)]
struct ChangeValue {
    messages: Option<Vec<InboundMessage>>,
}

#[derive(Deserialize)]
struct InboundMessage {
    from: String,
    id: String,
    #[serde(rename = "type")]
    message_type: String,
    text: Option<TextContent>,
}

#[derive(Deserialize)]
struct TextContent {
    body: String,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return vec![text.to_string()];
    }
    // Split on char boundaries to avoid breaking UTF-8 sequences.
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(max_chars)
        .map(|c| c.iter().collect())
        .collect()
}

fn required_env(key: &str) -> Result<String> {
    let val = std::env::var(key).map_err(|_| anyhow!("missing required env var: {}", key))?;
    anyhow::ensure!(!val.is_empty(), "env var {} is empty", key);
    Ok(val)
}
