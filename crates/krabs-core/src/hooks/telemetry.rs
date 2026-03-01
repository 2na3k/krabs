use crate::hooks::hook::{Hook, HookEvent, HookOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn event_type_str(event: &HookEvent) -> &'static str {
    match event {
        HookEvent::AgentStart { .. } => "agent_start",
        HookEvent::AgentStop { .. } => "agent_stop",
        HookEvent::TurnStart { .. } => "turn_start",
        HookEvent::TurnEnd { .. } => "turn_end",
        HookEvent::PreToolUse { .. } => "pre_tool_use",
        HookEvent::PostToolUse { .. } => "post_tool_use",
        HookEvent::PostToolUseFailure { .. } => "post_tool_use_failure",
    }
}

#[derive(Serialize)]
struct TelemetryEnvelope<'a> {
    event_type: &'static str,
    timestamp_ms: u64,
    session_id: Option<&'a str>,
    agent_id: Option<&'a str>,
    payload: &'a HookEvent,
}

impl<'a> TelemetryEnvelope<'a> {
    fn new(event: &'a HookEvent, session_id: Option<&'a str>, agent_id: Option<&'a str>) -> Self {
        Self {
            event_type: event_type_str(event),
            timestamp_ms: unix_millis(),
            session_id,
            agent_id,
            payload: event,
        }
    }
}

/// A hook that exports all agent lifecycle events to up to three backends:
/// HTTP/JSON, an mpsc channel, and a JSONL file.
pub struct TelemetryHook {
    http_endpoint: Option<Arc<str>>,
    channel_tx: Option<mpsc::Sender<String>>,
    jsonl_path: Option<Arc<PathBuf>>,
    session_id: Option<Arc<str>>,
    agent_id: Option<Arc<str>>,
    http_client: Arc<reqwest::Client>,
}

impl TelemetryHook {
    /// Returns the default JSONL path for a given session ID:
    /// `/tmp/krabs-telemetry-<session_id>.jsonl`
    pub fn default_jsonl_path(session_id: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/krabs-telemetry-{session_id}.jsonl"))
    }
}

/// Builder for [`TelemetryHook`].
pub struct TelemetryHookBuilder {
    http_endpoint: Option<String>,
    channel_tx: Option<mpsc::Sender<String>>,
    jsonl_path: Option<PathBuf>,
    session_id: Option<String>,
    agent_id: Option<String>,
}

impl TelemetryHookBuilder {
    pub fn new() -> Self {
        Self {
            http_endpoint: None,
            channel_tx: None,
            jsonl_path: None,
            session_id: None,
            agent_id: None,
        }
    }

    pub fn http_endpoint(mut self, url: impl Into<String>) -> Self {
        self.http_endpoint = Some(url.into());
        self
    }

    pub fn channel(mut self, tx: mpsc::Sender<String>) -> Self {
        self.channel_tx = Some(tx);
        self
    }

    pub fn jsonl_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.jsonl_path = Some(path.into());
        self
    }

    pub fn session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    pub fn agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    pub fn build(self) -> TelemetryHook {
        TelemetryHook {
            http_endpoint: self.http_endpoint.map(|s| Arc::from(s.as_str())),
            channel_tx: self.channel_tx,
            jsonl_path: self.jsonl_path.map(Arc::new),
            session_id: self.session_id.map(|s| Arc::from(s.as_str())),
            agent_id: self.agent_id.map(|s| Arc::from(s.as_str())),
            http_client: Arc::new(reqwest::Client::new()),
        }
    }
}

impl Default for TelemetryHookBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Hook for TelemetryHook {
    async fn on_event(&self, event: &HookEvent) -> Result<HookOutput> {
        let envelope =
            TelemetryEnvelope::new(event, self.session_id.as_deref(), self.agent_id.as_deref());
        let json = serde_json::to_string(&envelope)?;

        // HTTP backend: fire-and-forget
        if let Some(url) = &self.http_endpoint {
            let client = Arc::clone(&self.http_client);
            let url = Arc::clone(url);
            let body = json.clone();
            tokio::spawn(async move {
                let _ = client
                    .post(url.as_ref())
                    .header("Content-Type", "application/json")
                    .body(body)
                    .send()
                    .await;
            });
        }

        // Channel backend: non-blocking, drops if full
        if let Some(tx) = &self.channel_tx {
            let _ = tx.try_send(json.clone());
        }

        // JSONL backend: fire-and-forget append
        if let Some(path) = &self.jsonl_path {
            let path = Arc::clone(path);
            let line = format!("{json}\n");
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                if let Ok(mut f) = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path.as_ref())
                    .await
                {
                    let _ = f.write_all(line.as_bytes()).await;
                }
            });
        }

        Ok(HookOutput::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn sample_event() -> HookEvent {
        HookEvent::AgentStart {
            task: "test task".to_string(),
        }
    }

    #[test]
    fn event_type_str_all_variants() {
        assert_eq!(
            event_type_str(&HookEvent::AgentStart {
                task: String::new()
            }),
            "agent_start"
        );
        assert_eq!(
            event_type_str(&HookEvent::AgentStop {
                result: String::new()
            }),
            "agent_stop"
        );
        assert_eq!(
            event_type_str(&HookEvent::TurnStart { turn: 0 }),
            "turn_start"
        );
        assert_eq!(event_type_str(&HookEvent::TurnEnd { turn: 0 }), "turn_end");
        assert_eq!(
            event_type_str(&HookEvent::PreToolUse {
                tool_name: String::new(),
                args: Value::Null,
                tool_use_id: String::new(),
            }),
            "pre_tool_use"
        );
        assert_eq!(
            event_type_str(&HookEvent::PostToolUse {
                tool_name: String::new(),
                args: Value::Null,
                result: String::new(),
                tool_use_id: String::new(),
            }),
            "post_tool_use"
        );
        assert_eq!(
            event_type_str(&HookEvent::PostToolUseFailure {
                tool_name: String::new(),
                args: Value::Null,
                error: String::new(),
                tool_use_id: String::new(),
            }),
            "post_tool_use_failure"
        );
    }

    #[test]
    fn unix_millis_is_nonzero() {
        assert!(unix_millis() > 0);
    }

    #[test]
    fn envelope_serializes_correctly() {
        let event = sample_event();
        let envelope = TelemetryEnvelope::new(&event, Some("sess-1"), Some("agent-1"));
        let json = serde_json::to_string(&envelope).unwrap();
        let val: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["event_type"], "agent_start");
        assert!(val["timestamp_ms"].as_u64().unwrap() > 0);
        assert_eq!(val["session_id"], "sess-1");
        assert_eq!(val["agent_id"], "agent-1");
        assert!(val["payload"].is_object());
    }

    #[test]
    fn builder_default_is_all_none() {
        let hook = TelemetryHookBuilder::new().build();
        assert!(hook.http_endpoint.is_none());
        assert!(hook.channel_tx.is_none());
        assert!(hook.jsonl_path.is_none());
        assert!(hook.session_id.is_none());
        assert!(hook.agent_id.is_none());
    }

    #[test]
    fn builder_sets_http_endpoint() {
        let hook = TelemetryHookBuilder::new()
            .http_endpoint("http://localhost:9000/events")
            .build();
        assert_eq!(
            hook.http_endpoint.as_deref(),
            Some("http://localhost:9000/events")
        );
    }

    #[test]
    fn builder_sets_channel() {
        let (tx, _rx) = mpsc::channel::<String>(1);
        let hook = TelemetryHookBuilder::new().channel(tx).build();
        assert!(hook.channel_tx.is_some());
    }

    #[test]
    fn builder_sets_jsonl_path() {
        let hook = TelemetryHookBuilder::new()
            .jsonl_path("/tmp/test.jsonl")
            .build();
        assert_eq!(
            hook.jsonl_path.as_deref().map(|p| p.as_os_str()),
            Some(PathBuf::from("/tmp/test.jsonl").as_os_str())
        );
    }

    #[test]
    fn builder_sets_session_and_agent_id() {
        let hook = TelemetryHookBuilder::new()
            .session_id("sess-abc")
            .agent_id("agent-xyz")
            .build();
        assert_eq!(hook.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(hook.agent_id.as_deref(), Some("agent-xyz"));
    }

    #[test]
    fn default_jsonl_path_format() {
        let path = TelemetryHook::default_jsonl_path("abc");
        assert_eq!(path, PathBuf::from("/tmp/krabs-telemetry-abc.jsonl"));
    }

    #[tokio::test]
    async fn hook_returns_continue_no_backends() {
        let hook = TelemetryHookBuilder::new().build();
        let result = hook.on_event(&sample_event()).await.unwrap();
        assert!(matches!(result, HookOutput::Continue));
    }

    #[tokio::test]
    async fn channel_backend_receives_json() {
        let (tx, mut rx) = mpsc::channel::<String>(10);
        let hook = TelemetryHookBuilder::new().channel(tx).build();
        hook.on_event(&sample_event()).await.unwrap();
        let msg = rx.recv().await.expect("channel should receive a message");
        assert!(!msg.is_empty());
        // Must be valid JSON
        let _: Value = serde_json::from_str(&msg).unwrap();
    }

    #[tokio::test]
    async fn channel_backend_json_shape() {
        let (tx, mut rx) = mpsc::channel::<String>(10);
        let hook = TelemetryHookBuilder::new()
            .channel(tx)
            .session_id("s1")
            .build();
        hook.on_event(&sample_event()).await.unwrap();
        let msg = rx.recv().await.unwrap();
        let val: Value = serde_json::from_str(&msg).unwrap();
        assert!(val["event_type"].is_string());
        assert!(val["timestamp_ms"].is_number());
        assert_eq!(val["session_id"], "s1");
        assert!(val["payload"].is_object());
    }

    #[tokio::test]
    async fn channel_backend_full_buffer_does_not_block() {
        let (tx, _rx) = mpsc::channel::<String>(1);
        // Fill the buffer
        let _ = tx.try_send("existing".to_string());
        let hook = TelemetryHookBuilder::new().channel(tx).build();
        // Should not block even though buffer is full
        let result = hook.on_event(&sample_event()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn jsonl_backend_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let hook = TelemetryHookBuilder::new().jsonl_path(&path).build();
        hook.on_event(&sample_event()).await.unwrap();
        // Give the spawned task time to write
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(path.exists());
    }

    #[tokio::test]
    async fn jsonl_backend_appends_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("append.jsonl");
        let hook = TelemetryHookBuilder::new().jsonl_path(&path).build();
        hook.on_event(&sample_event()).await.unwrap();
        hook.on_event(&HookEvent::AgentStop {
            result: "done".to_string(),
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content.lines().count(), 2);
    }

    #[tokio::test]
    async fn jsonl_backend_line_is_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("valid.jsonl");
        let hook = TelemetryHookBuilder::new().jsonl_path(&path).build();
        hook.on_event(&sample_event()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        for line in content.lines() {
            let _: Value = serde_json::from_str(line).unwrap();
        }
    }

    #[tokio::test]
    async fn http_backend_unreachable_does_not_error() {
        // Port 19999 is very unlikely to have anything listening
        let hook = TelemetryHookBuilder::new()
            .http_endpoint("http://127.0.0.1:19999/events")
            .build();
        let result = hook.on_event(&sample_event()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn all_backends_fire_simultaneously() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("all.jsonl");
        let (tx, mut rx) = mpsc::channel::<String>(10);
        let hook = TelemetryHookBuilder::new()
            .channel(tx)
            .jsonl_path(&path)
            .http_endpoint("http://127.0.0.1:19998/events")
            .build();
        let result = hook.on_event(&sample_event()).await;
        assert!(result.is_ok());
        // Channel should have received
        let msg = rx.recv().await.unwrap();
        let _: Value = serde_json::from_str(&msg).unwrap();
        // File should be created
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(path.exists());
    }

    #[tokio::test]
    async fn all_event_variants_through_channel() {
        let (tx, mut rx) = mpsc::channel::<String>(20);
        let hook = TelemetryHookBuilder::new().channel(tx).build();

        let events = vec![
            HookEvent::AgentStart {
                task: "t".to_string(),
            },
            HookEvent::AgentStop {
                result: "r".to_string(),
            },
            HookEvent::TurnStart { turn: 0 },
            HookEvent::TurnEnd { turn: 0 },
            HookEvent::PreToolUse {
                tool_name: "bash".to_string(),
                args: Value::Null,
                tool_use_id: "id1".to_string(),
            },
            HookEvent::PostToolUse {
                tool_name: "bash".to_string(),
                args: Value::Null,
                result: "ok".to_string(),
                tool_use_id: "id1".to_string(),
            },
            HookEvent::PostToolUseFailure {
                tool_name: "bash".to_string(),
                args: Value::Null,
                error: "err".to_string(),
                tool_use_id: "id1".to_string(),
            },
        ];

        for event in &events {
            hook.on_event(event).await.unwrap();
        }

        let mut count = 0;
        while let Ok(msg) = rx.try_recv() {
            let _: Value = serde_json::from_str(&msg).unwrap();
            count += 1;
        }
        assert_eq!(count, 7);
    }

    #[tokio::test]
    async fn http_backend_posts_to_endpoint() {
        use std::convert::Infallible;
        use std::net::SocketAddr;

        let received = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let received_clone = Arc::clone(&received);

        // Minimal hyper server
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                use hyper::service::service_fn;
                use hyper_util::rt::TokioIo;

                let received_inner = Arc::clone(&received_clone);
                let io = TokioIo::new(stream);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                            let received_inner = Arc::clone(&received_inner);
                            async move {
                                use http_body_util::BodyExt;
                                let body = req.collect().await.unwrap().to_bytes();
                                let text = String::from_utf8_lossy(&body).to_string();
                                received_inner.lock().await.push(text);
                                Ok::<_, Infallible>(hyper::Response::new(http_body_util::Empty::<
                                    hyper::body::Bytes,
                                >::new(
                                )))
                            }
                        }),
                    )
                    .await;
            }
        });

        let hook = TelemetryHookBuilder::new()
            .http_endpoint(format!("http://{addr}/events"))
            .build();
        hook.on_event(&sample_event()).await.unwrap();

        // Wait for the fire-and-forget spawn to complete
        tokio::time::sleep(Duration::from_millis(200)).await;

        let msgs = received.lock().await;
        assert_eq!(msgs.len(), 1);
        let _: Value = serde_json::from_str(&msgs[0]).unwrap();
    }
}
