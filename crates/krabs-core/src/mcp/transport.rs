use anyhow::{bail, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tracing::debug;

use super::jsonrpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};

// ── Stdio transport ──────────────────────────────────────────────────────────

pub struct StdioTransport {
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    id_counter: AtomicU64,
    _child: Mutex<Child>,
}

impl StdioTransport {
    pub fn spawn(command: &str, args: &[String]) -> Result<Self> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin from MCP process"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdout from MCP process"))?;

        Ok(Self {
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            id_counter: AtomicU64::new(1),
            _child: Mutex::new(child),
        })
    }

    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(id, method, params);
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        debug!("MCP stdio → {}", line.trim());

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }

        // Read lines until we find a response matching our id
        loop {
            let mut buf = String::new();
            {
                let mut stdout = self.stdout.lock().await;
                stdout.read_line(&mut buf).await?;
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                bail!("MCP server closed connection");
            }
            debug!("MCP stdio ← {}", trimmed);

            // Skip notifications (no "id" field or id is null)
            let raw: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if raw.get("id").is_none() || raw["id"].is_null() {
                continue; // notification
            }

            let resp: JsonRpcResponse = serde_json::from_value(raw)?;
            if resp.id != Some(id) {
                continue;
            }

            if let Some(err) = resp.error {
                bail!("MCP error {}: {}", err.code, err.message);
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let notif = JsonRpcNotification::new(method, params);
        let mut line = serde_json::to_string(&notif)?;
        line.push('\n');
        debug!("MCP stdio notify → {}", line.trim());
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}

// ── SSE/HTTP transport ───────────────────────────────────────────────────────

/// HTTP+SSE transport for remote MCP servers.
///
/// Protocol:
/// 1. GET `{url}` — establishes SSE stream; first event contains the session endpoint URL.
/// 2. POST `{endpoint}` with JSON-RPC body — server pushes response over the SSE stream.
///
/// This implementation uses a simpler request-response approach:
/// POST JSON-RPC to `{url}/message` and receive the response synchronously.
/// Full SSE session management is left for future work.
pub struct SseTransport {
    client: Client,
    base_url: String,
    id_counter: AtomicU64,
}

impl SseTransport {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.into(),
            id_counter: AtomicU64::new(1),
        }
    }

    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(id, method, params);
        let url = format!("{}/message", self.base_url.trim_end_matches('/'));
        debug!("MCP SSE → POST {} {:?}", url, method);

        let response = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await?
            .error_for_status()?;

        // Try to read SSE events until we get our response
        let mut stream = response.bytes_stream();
        let mut buf = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buf.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete SSE lines
            while let Some(pos) = buf.find("\n\n") {
                let event_block = buf[..pos].to_string();
                buf.drain(..pos + 2);

                for line in event_block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        debug!("MCP SSE ← {}", data);
                        let resp: JsonRpcResponse = match serde_json::from_str(data) {
                            Ok(r) => r,
                            Err(_) => continue,
                        };
                        if resp.id != Some(id) {
                            continue;
                        }
                        if let Some(err) = resp.error {
                            bail!("MCP error {}: {}", err.code, err.message);
                        }
                        return Ok(resp.result.unwrap_or(Value::Null));
                    }
                }
            }
        }

        bail!("SSE stream ended without a matching response for id={}", id)
    }
}

// ── Unified transport enum ───────────────────────────────────────────────────

pub enum Transport {
    Stdio(Box<StdioTransport>),
    Sse(SseTransport),
}

impl Transport {
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        match self {
            Self::Stdio(t) => t.request(method, params).await,
            Self::Sse(t) => t.request(method, params).await,
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        match self {
            Self::Stdio(t) => t.notify(method, params).await,
            // Notifications over SSE are fire-and-forget POSTs
            Self::Sse(t) => {
                let notif = JsonRpcNotification::new(method, params);
                let url = format!("{}/message", t.base_url.trim_end_matches('/'));
                let _ = t.client.post(&url).json(&notif).send().await;
                Ok(())
            }
        }
    }
}
