use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::stream;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex, RwLock};
use uuid::Uuid;

use crate::protocol::jsonrpc::{JsonRpcNotification, JsonRpcRequest};
use crate::server::handler::dispatch;
use crate::tools::registry::McpToolRegistry;

pub type SessionMap = Arc<Mutex<HashMap<Uuid, mpsc::Sender<String>>>>;
type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

/// Cloneable handle for pushing notifications to all connected SSE clients.
#[derive(Clone)]
pub struct NotificationBroadcaster {
    sessions: SessionMap,
}

impl NotificationBroadcaster {
    pub(crate) fn new(sessions: SessionMap) -> Self {
        Self { sessions }
    }

    /// Broadcast a notification to every connected SSE session.
    pub async fn broadcast(&self, notification: &JsonRpcNotification) {
        let Ok(msg) = serde_json::to_string(notification) else {
            return;
        };
        let sessions = self.sessions.lock().await;
        for tx in sessions.values() {
            tx.send(msg.clone()).await.ok();
        }
    }

    /// Send a notification to a single session by ID.
    pub async fn broadcast_to(&self, session_id: Uuid, notification: &JsonRpcNotification) {
        let Ok(msg) = serde_json::to_string(notification) else {
            return;
        };
        if let Some(tx) = self.sessions.lock().await.get(&session_id) {
            tx.send(msg).await.ok();
        }
    }

    /// Number of currently connected SSE sessions.
    pub async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }
}

/// Accept loop. Called from `McpServer::run_sse` after the handle is returned.
pub async fn run_sse_loop(
    registry: Arc<RwLock<McpToolRegistry>>,
    server_name: String,
    server_version: String,
    addr: SocketAddr,
    sessions: SessionMap,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("MCP SSE server listening on {addr}");

    loop {
        let (stream, _) = listener.accept().await?;
        let sessions = sessions.clone();
        let registry = registry.clone();
        let server_name = server_name.clone();
        let server_version = server_version.clone();

        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        handle_request(
                            req,
                            sessions.clone(),
                            registry.clone(),
                            server_name.clone(),
                            server_version.clone(),
                        )
                    }),
                )
                .await
                .ok();
        });
    }
}

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    sessions: SessionMap,
    registry: Arc<RwLock<McpToolRegistry>>,
    server_name: String,
    server_version: String,
) -> Result<Response<BoxBody>, hyper::Error> {
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();

    match (req.method().clone(), path.as_str()) {
        (Method::GET, "/sse") => Ok(handle_sse(sessions).await),
        (Method::POST, "/message") => {
            Ok(handle_message(req, query, sessions, registry, server_name, server_version).await)
        }
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(full_body("Not found"))
            .unwrap()),
    }
}

async fn handle_sse(sessions: SessionMap) -> Response<BoxBody> {
    let session_id = Uuid::new_v4();
    let (tx, rx) = mpsc::channel::<String>(32);

    // Keep one clone only for closed() detection; the map holds the real sender.
    let tx_watcher = tx.clone();
    sessions.lock().await.insert(session_id, tx);

    // Spawn a task that removes the session once the receiver is dropped
    // (i.e. when the client disconnects and hyper drops the response body).
    let sessions_cleanup = sessions.clone();
    tokio::spawn(async move {
        tx_watcher.closed().await;
        sessions_cleanup.lock().await.remove(&session_id);
        tracing::debug!("SSE session {session_id} cleaned up");
    });

    let endpoint_event = format!("event: endpoint\ndata: /message?sessionId={session_id}\n\n");

    let init = stream::once(async move {
        Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from(endpoint_event)))
    });

    let events = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|msg| {
            let frame_data = format!("event: message\ndata: {msg}\n\n");
            (
                Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from(frame_data))),
                rx,
            )
        })
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(BoxBody::new(StreamBody::new(init.chain(events))))
        .unwrap()
}

async fn handle_message(
    req: Request<hyper::body::Incoming>,
    query: String,
    sessions: SessionMap,
    registry: Arc<RwLock<McpToolRegistry>>,
    server_name: String,
    server_version: String,
) -> Response<BoxBody> {
    let session_id = match parse_session_id(&query) {
        Some(id) => id,
        None => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(full_body("Missing or invalid sessionId"))
                .unwrap();
        }
    };

    let tx = sessions.lock().await.get(&session_id).cloned();
    let tx = match tx {
        Some(t) => t,
        None => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(full_body("Unknown sessionId"))
                .unwrap();
        }
    };

    let body_bytes = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(full_body("Failed to read body"))
                .unwrap();
        }
    };

    let rpc_req: JsonRpcRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(full_body("Invalid JSON-RPC request"))
                .unwrap();
        }
    };

    if let Some(response) = dispatch(&registry, &server_name, &server_version, rpc_req).await {
        if let Ok(serialized) = serde_json::to_string(&response) {
            tx.send(serialized).await.ok();
        }
    }

    Response::builder()
        .status(StatusCode::ACCEPTED)
        .body(full_body(""))
        .unwrap()
}

fn parse_session_id(query: &str) -> Option<Uuid> {
    for part in query.split('&') {
        if let Some(val) = part.strip_prefix("sessionId=") {
            return val.parse().ok();
        }
    }
    None
}

fn full_body(text: &'static str) -> BoxBody {
    Full::new(Bytes::from(text))
        .map_err(|never| match never {})
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::jsonrpc::JsonRpcNotification;

    fn make_sessions() -> SessionMap {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[tokio::test]
    async fn test_broadcast_reaches_all_sessions() {
        let sessions = make_sessions();
        let broadcaster = NotificationBroadcaster::new(sessions.clone());

        let (tx1, mut rx1) = mpsc::channel(4);
        let (tx2, mut rx2) = mpsc::channel(4);
        sessions.lock().await.insert(Uuid::new_v4(), tx1);
        sessions.lock().await.insert(Uuid::new_v4(), tx2);

        let notif = JsonRpcNotification::new("notifications/tools/list_changed");
        broadcaster.broadcast(&notif).await;

        let msg1 = rx1.recv().await.unwrap();
        let msg2 = rx2.recv().await.unwrap();
        assert!(msg1.contains("tools/list_changed"));
        assert!(msg2.contains("tools/list_changed"));
    }

    #[tokio::test]
    async fn test_broadcast_to_specific_session() {
        let sessions = make_sessions();
        let broadcaster = NotificationBroadcaster::new(sessions.clone());

        let (tx_target, mut rx_target) = mpsc::channel(4);
        let (tx_other, mut rx_other) = mpsc::channel(4);
        let target_id = Uuid::new_v4();
        sessions.lock().await.insert(target_id, tx_target);
        sessions.lock().await.insert(Uuid::new_v4(), tx_other);

        let notif = JsonRpcNotification::new("notifications/tools/list_changed");
        broadcaster.broadcast_to(target_id, &notif).await;

        let msg = rx_target.recv().await.unwrap();
        assert!(msg.contains("tools/list_changed"));

        // other session receives nothing
        assert!(rx_other.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_session_count() {
        let sessions = make_sessions();
        let broadcaster = NotificationBroadcaster::new(sessions.clone());

        assert_eq!(broadcaster.session_count().await, 0);
        let (tx, _rx) = mpsc::channel(1);
        sessions.lock().await.insert(Uuid::new_v4(), tx);
        assert_eq!(broadcaster.session_count().await, 1);
    }

    #[tokio::test]
    async fn test_cleanup_on_disconnect() {
        let sessions = make_sessions();
        let broadcaster = NotificationBroadcaster::new(sessions.clone());

        let (tx, rx) = mpsc::channel::<String>(4);
        let tx_watcher = tx.clone();
        let session_id = Uuid::new_v4();
        sessions.lock().await.insert(session_id, tx);

        let sessions_cleanup = sessions.clone();
        tokio::spawn(async move {
            tx_watcher.closed().await;
            sessions_cleanup.lock().await.remove(&session_id);
        });

        assert_eq!(broadcaster.session_count().await, 1);

        // Simulate disconnect: drop the receiver
        drop(rx);

        // Give the cleanup task a moment to run
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        assert_eq!(broadcaster.session_count().await, 0);
    }
}
