//! Langfuse tracing hook.
//!
//! Maps every [`HookEvent`] to the Langfuse batch ingestion API so that agent
//! runs appear as traces in the Langfuse UI with nested spans per turn and per
//! tool call.
//!
//! # Mapping
//! | HookEvent            | Langfuse event type  | Notes                                  |
//! |----------------------|----------------------|----------------------------------------|
//! | `AgentStart`         | `trace-create`       | Creates the root trace                 |
//! | `TurnStart`          | `span-create`        | One span per turn, child of trace      |
//! | `PreToolUse`         | `span-create`        | Child of the current turn span         |
//! | `PostToolUse`        | `span-update`        | Closes tool span with output           |
//! | `PostToolUseFailure` | `span-update`        | Closes tool span with ERROR level      |
//! | `TurnEnd`            | `span-update`        | Closes the turn span                   |
//! | `AgentStop`          | `trace-create`       | Upserts trace with final output        |
//!
//! # Usage
//! ```rust,no_run
//! use krabs_core::hooks::langfuse::LangfuseHookBuilder;
//! use std::sync::Arc;
//!
//! let hook = LangfuseHookBuilder::new(
//!     "pk-lf-...",   // Langfuse public key
//!     "sk-lf-...",   // Langfuse secret key
//! )
//! .base_url("http://localhost:3000")
//! .session_id("my-session")
//! .build();
//! ```

use crate::hooks::hook::{Hook, HookEvent, HookOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

fn now_iso() -> String {
    // RFC 3339 / ISO 8601 with millisecond precision
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let secs = ms / 1000;
    let millis = ms % 1000;
    // Format as 1970-01-01T00:00:00.000Z — use chrono-free manual approach
    // We rely on the fact that reqwest/serde_json don't care as long as it's valid ISO 8601.
    // Using a simple offset calculation is error-prone; emit Unix epoch offset instead via
    // the standard library's SystemTime formatting via httpdate or just use a numeric string.
    // Since Langfuse accepts any ISO 8601, we use the well-known trick of printing duration
    // from epoch as a datetime string using integer arithmetic.
    epoch_ms_to_iso(secs as u64, millis as u32)
}

fn epoch_ms_to_iso(secs: u64, ms: u32) -> String {
    // Convert Unix timestamp to ISO 8601 without external deps.
    let (year, month, day, hour, min, sec) = unix_secs_to_ymd_hms(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, min, sec, ms
    )
}

fn unix_secs_to_ymd_hms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    // Days since Unix epoch
    let days = secs / 86400;
    let rem = secs % 86400;
    let hour = (rem / 3600) as u32;
    let min = ((rem % 3600) / 60) as u32;
    let sec = (rem % 60) as u32;

    // Civil date from day count (Gregorian)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (y as u32, m as u32, d as u32, hour, min, sec)
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Internal state shared across async on_event calls
// ---------------------------------------------------------------------------

#[derive(Default)]
struct LangfuseState {
    /// Root trace ID created on AgentStart
    trace_id: Option<String>,
    /// turn index → span ID
    turn_spans: HashMap<usize, String>,
    /// tool_use_id → span ID
    tool_spans: HashMap<String, String>,
    /// Most recent open turn index (for parenting tool spans)
    current_turn: Option<usize>,
}

// ---------------------------------------------------------------------------
// Langfuse batch ingestion payload helpers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct BatchPayload {
    batch: Vec<Value>,
}

impl BatchPayload {
    fn single(event: Value) -> Self {
        Self {
            batch: vec![event],
        }
    }
}

fn make_event(event_type: &str, body: Value) -> Value {
    json!({
        "id": new_id(),
        "timestamp": now_iso(),
        "type": event_type,
        "body": body,
    })
}

// ---------------------------------------------------------------------------
// Public structs
// ---------------------------------------------------------------------------

/// A hook that sends agent lifecycle events to a Langfuse instance as traces
/// and spans via the batch ingestion API.
pub struct LangfuseHook {
    public_key: String,
    secret_key: String,
    base_url: String,
    session_id: Option<String>,
    agent_id: Option<String>,
    client: Arc<reqwest::Client>,
    state: Arc<Mutex<LangfuseState>>,
}

impl LangfuseHook {
    fn ingestion_url(&self) -> String {
        format!("{}/api/public/ingestion", self.base_url.trim_end_matches('/'))
    }

    async fn send(&self, payload: BatchPayload) {
        let url = self.ingestion_url();
        let client = Arc::clone(&self.client);
        let pk = self.public_key.clone();
        let sk = self.secret_key.clone();
        tokio::spawn(async move {
            let _ = client
                .post(&url)
                .basic_auth(pk, Some(sk))
                .json(&payload)
                .send()
                .await;
        });
    }
}

/// Builder for [`LangfuseHook`].
pub struct LangfuseHookBuilder {
    public_key: String,
    secret_key: String,
    base_url: String,
    session_id: Option<String>,
    agent_id: Option<String>,
}

impl LangfuseHookBuilder {
    /// Create a new builder. `public_key` and `secret_key` are the Langfuse
    /// project credentials (found in project settings → API keys).
    pub fn new(public_key: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self {
            public_key: public_key.into(),
            secret_key: secret_key.into(),
            base_url: "http://localhost:3000".to_string(),
            session_id: None,
            agent_id: None,
        }
    }

    /// Override the Langfuse base URL. Defaults to `http://localhost:3000`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Tag every trace and span with this session ID.
    pub fn session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    /// Tag every trace with this agent ID (stored as `userId`).
    pub fn agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    pub fn build(self) -> LangfuseHook {
        LangfuseHook {
            public_key: self.public_key,
            secret_key: self.secret_key,
            base_url: self.base_url,
            session_id: self.session_id,
            agent_id: self.agent_id,
            client: Arc::new(reqwest::Client::new()),
            state: Arc::new(Mutex::new(LangfuseState::default())),
        }
    }
}

// ---------------------------------------------------------------------------
// Hook implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Hook for LangfuseHook {
    async fn on_event(&self, event: &HookEvent) -> Result<HookOutput> {
        match event {
            // ------------------------------------------------------------------
            // AgentStart → trace-create
            // ------------------------------------------------------------------
            HookEvent::AgentStart { task } => {
                let trace_id = new_id();
                {
                    let mut state = self.state.lock().await;
                    state.trace_id = Some(trace_id.clone());
                    state.turn_spans.clear();
                    state.tool_spans.clear();
                    state.current_turn = None;
                }
                let mut body = json!({
                    "id": trace_id,
                    "name": "agent-run",
                    "input": task,
                    "tags": ["krabs"],
                });
                if let Some(sid) = &self.session_id {
                    body["sessionId"] = json!(sid);
                }
                if let Some(aid) = &self.agent_id {
                    body["userId"] = json!(aid);
                }
                self.send(BatchPayload::single(make_event("trace-create", body)))
                    .await;
            }

            // ------------------------------------------------------------------
            // TurnStart → span-create (child of trace)
            // ------------------------------------------------------------------
            HookEvent::TurnStart { turn } => {
                let state = self.state.lock().await;
                let trace_id = match &state.trace_id {
                    Some(id) => id.clone(),
                    None => return Ok(HookOutput::Continue),
                };
                drop(state);

                let span_id = new_id();
                {
                    let mut state = self.state.lock().await;
                    state.turn_spans.insert(*turn, span_id.clone());
                    state.current_turn = Some(*turn);
                }
                let body = json!({
                    "id": span_id,
                    "traceId": trace_id,
                    "name": format!("turn-{turn}"),
                    "startTime": now_iso(),
                    "metadata": { "turn": turn },
                });
                self.send(BatchPayload::single(make_event("span-create", body)))
                    .await;
            }

            // ------------------------------------------------------------------
            // PreToolUse → span-create (child of current turn span)
            // ------------------------------------------------------------------
            HookEvent::PreToolUse {
                tool_name,
                args,
                tool_use_id,
            } => {
                let state = self.state.lock().await;
                let trace_id = match &state.trace_id {
                    Some(id) => id.clone(),
                    None => return Ok(HookOutput::Continue),
                };
                let parent_id = state
                    .current_turn
                    .and_then(|t| state.turn_spans.get(&t))
                    .cloned();
                drop(state);

                let span_id = new_id();
                {
                    let mut state = self.state.lock().await;
                    state.tool_spans.insert(tool_use_id.clone(), span_id.clone());
                }
                let mut body = json!({
                    "id": span_id,
                    "traceId": trace_id,
                    "name": tool_name,
                    "startTime": now_iso(),
                    "input": args,
                    "metadata": { "tool_use_id": tool_use_id },
                });
                if let Some(pid) = parent_id {
                    body["parentObservationId"] = json!(pid);
                }
                self.send(BatchPayload::single(make_event("span-create", body)))
                    .await;
            }

            // ------------------------------------------------------------------
            // PostToolUse → span-update (close tool span with output)
            // ------------------------------------------------------------------
            HookEvent::PostToolUse {
                result,
                tool_use_id,
                ..
            } => {
                let state = self.state.lock().await;
                let span_id = match state.tool_spans.get(tool_use_id) {
                    Some(id) => id.clone(),
                    None => return Ok(HookOutput::Continue),
                };
                drop(state);

                let body = json!({
                    "id": span_id,
                    "output": result,
                    "endTime": now_iso(),
                    "level": "DEFAULT",
                });
                self.send(BatchPayload::single(make_event("span-update", body)))
                    .await;
            }

            // ------------------------------------------------------------------
            // PostToolUseFailure → span-update (close tool span with ERROR)
            // ------------------------------------------------------------------
            HookEvent::PostToolUseFailure {
                error,
                tool_use_id,
                ..
            } => {
                let state = self.state.lock().await;
                let span_id = match state.tool_spans.get(tool_use_id) {
                    Some(id) => id.clone(),
                    None => return Ok(HookOutput::Continue),
                };
                drop(state);

                let body = json!({
                    "id": span_id,
                    "output": error,
                    "endTime": now_iso(),
                    "level": "ERROR",
                    "statusMessage": error,
                });
                self.send(BatchPayload::single(make_event("span-update", body)))
                    .await;
            }

            // ------------------------------------------------------------------
            // TurnEnd → span-update (close turn span)
            // ------------------------------------------------------------------
            HookEvent::TurnEnd { turn } => {
                let state = self.state.lock().await;
                let span_id = match state.turn_spans.get(turn) {
                    Some(id) => id.clone(),
                    None => return Ok(HookOutput::Continue),
                };
                drop(state);

                let body = json!({
                    "id": span_id,
                    "endTime": now_iso(),
                });
                self.send(BatchPayload::single(make_event("span-update", body)))
                    .await;
            }

            // ------------------------------------------------------------------
            // AgentStop → trace-create (upsert with output)
            // ------------------------------------------------------------------
            HookEvent::AgentStop { result } => {
                let state = self.state.lock().await;
                let trace_id = match &state.trace_id {
                    Some(id) => id.clone(),
                    None => return Ok(HookOutput::Continue),
                };
                drop(state);

                let mut body = json!({
                    "id": trace_id,
                    "output": result,
                });
                if let Some(sid) = &self.session_id {
                    body["sessionId"] = json!(sid);
                }
                if let Some(aid) = &self.agent_id {
                    body["userId"] = json!(aid);
                }
                self.send(BatchPayload::single(make_event("trace-create", body)))
                    .await;
            }
        }

        Ok(HookOutput::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_iso_looks_like_iso8601() {
        let s = now_iso();
        // e.g. "2026-03-01T12:00:00.123Z"
        assert!(s.ends_with('Z'), "should end with Z: {s}");
        assert!(s.contains('T'), "should contain T: {s}");
        assert_eq!(s.len(), 24, "should be 24 chars: {s}");
    }

    #[test]
    fn epoch_zero_is_unix_epoch() {
        let s = epoch_ms_to_iso(0, 0);
        assert_eq!(s, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn known_timestamp() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        let s = epoch_ms_to_iso(1_704_067_200, 0);
        assert_eq!(s, "2024-01-01T00:00:00.000Z");
    }

    #[test]
    fn builder_defaults() {
        let hook = LangfuseHookBuilder::new("pk", "sk").build();
        assert_eq!(hook.public_key, "pk");
        assert_eq!(hook.secret_key, "sk");
        assert_eq!(hook.base_url, "http://localhost:3000");
        assert!(hook.session_id.is_none());
        assert!(hook.agent_id.is_none());
    }

    #[test]
    fn builder_sets_all_fields() {
        let hook = LangfuseHookBuilder::new("pk", "sk")
            .base_url("http://langfuse:3000")
            .session_id("sess-1")
            .agent_id("agent-1")
            .build();
        assert_eq!(hook.base_url, "http://langfuse:3000");
        assert_eq!(hook.session_id.as_deref(), Some("sess-1"));
        assert_eq!(hook.agent_id.as_deref(), Some("agent-1"));
    }

    #[test]
    fn ingestion_url_strips_trailing_slash() {
        let hook = LangfuseHookBuilder::new("pk", "sk")
            .base_url("http://localhost:3000/")
            .build();
        assert_eq!(hook.ingestion_url(), "http://localhost:3000/api/public/ingestion");
    }

    #[tokio::test]
    async fn agent_start_sets_trace_id() {
        let hook = LangfuseHookBuilder::new("pk", "sk").build();
        assert!(hook.state.lock().await.trace_id.is_none());
        hook.on_event(&HookEvent::AgentStart {
            task: "do thing".to_string(),
        })
        .await
        .unwrap();
        assert!(hook.state.lock().await.trace_id.is_some());
    }

    #[tokio::test]
    async fn turn_start_stores_span_id() {
        let hook = LangfuseHookBuilder::new("pk", "sk").build();
        // Seed a trace_id first
        hook.state.lock().await.trace_id = Some("trace-1".to_string());
        hook.on_event(&HookEvent::TurnStart { turn: 0 })
            .await
            .unwrap();
        let state = hook.state.lock().await;
        assert!(state.turn_spans.contains_key(&0));
        assert_eq!(state.current_turn, Some(0));
    }

    #[tokio::test]
    async fn pre_tool_use_stores_tool_span() {
        let hook = LangfuseHookBuilder::new("pk", "sk").build();
        {
            let mut state = hook.state.lock().await;
            state.trace_id = Some("trace-1".to_string());
            state.turn_spans.insert(0, "turn-span-1".to_string());
            state.current_turn = Some(0);
        }
        hook.on_event(&HookEvent::PreToolUse {
            tool_name: "bash".to_string(),
            args: serde_json::json!({"cmd": "ls"}),
            tool_use_id: "tool-1".to_string(),
        })
        .await
        .unwrap();
        let state = hook.state.lock().await;
        assert!(state.tool_spans.contains_key("tool-1"));
    }

    #[tokio::test]
    async fn no_trace_id_is_noop() {
        // No trace_id — all events should silently return Continue
        let hook = LangfuseHookBuilder::new("pk", "sk").build();
        let events = vec![
            HookEvent::TurnStart { turn: 0 },
            HookEvent::PreToolUse {
                tool_name: "bash".to_string(),
                args: serde_json::Value::Null,
                tool_use_id: "t1".to_string(),
            },
            HookEvent::PostToolUse {
                tool_name: "bash".to_string(),
                args: serde_json::Value::Null,
                result: "ok".to_string(),
                tool_use_id: "t1".to_string(),
            },
            HookEvent::TurnEnd { turn: 0 },
            HookEvent::AgentStop {
                result: "done".to_string(),
            },
        ];
        for ev in &events {
            let out = hook.on_event(ev).await.unwrap();
            assert!(matches!(out, HookOutput::Continue));
        }
    }

    #[tokio::test]
    async fn full_lifecycle_returns_continue() {
        let hook = LangfuseHookBuilder::new("pk", "sk")
            .session_id("s1")
            .agent_id("a1")
            .build();

        let events = vec![
            HookEvent::AgentStart {
                task: "test".to_string(),
            },
            HookEvent::TurnStart { turn: 0 },
            HookEvent::PreToolUse {
                tool_name: "bash".to_string(),
                args: serde_json::json!({}),
                tool_use_id: "tu-1".to_string(),
            },
            HookEvent::PostToolUse {
                tool_name: "bash".to_string(),
                args: serde_json::json!({}),
                result: "hello".to_string(),
                tool_use_id: "tu-1".to_string(),
            },
            HookEvent::TurnEnd { turn: 0 },
            HookEvent::AgentStop {
                result: "all done".to_string(),
            },
        ];

        for ev in &events {
            let out = hook.on_event(ev).await.unwrap();
            assert!(matches!(out, HookOutput::Continue));
        }

        let state = hook.state.lock().await;
        assert!(state.trace_id.is_some());
        assert!(state.turn_spans.contains_key(&0));
        assert!(state.tool_spans.contains_key("tu-1"));
    }

    #[tokio::test]
    async fn unreachable_server_does_not_error() {
        let hook = LangfuseHookBuilder::new("pk", "sk")
            .base_url("http://127.0.0.1:19997")
            .build();
        // Should always return Ok even if Langfuse is down
        let result = hook
            .on_event(&HookEvent::AgentStart {
                task: "t".to_string(),
            })
            .await;
        assert!(result.is_ok());
    }
}
