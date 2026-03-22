use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use krabs_core::{SessionOpts, StreamChunk};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

use crate::dto::ChatRequest;
use crate::error::ServerError;
use crate::event_bus::SessionEventBus;
use crate::hook::ServerHook;
use crate::state::AppState;

/// Send a message to an agent and receive streaming SSE response.
#[utoipa::path(
    post,
    path = "/api/v1/agents/{agent_id}/chat",
    params(
        ("agent_id" = String, Path, description = "Agent ID")
    ),
    request_body = ChatRequest,
    responses(
        (status = 200, description = "SSE stream of agent response"),
        (status = 404, description = "Agent not found"),
        (status = 409, description = "Agent is busy"),
    ),
    tag = "chat"
)]
pub async fn chat(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    Json(req): Json<ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ServerError> {
    let handle_mutex = state
        .agent_pool
        .get(&agent_id)
        .await
        .map_err(ServerError::from)?;

    // Acquire handle, begin turn, extract factory — then release lock
    let (turn_input, factory) = {
        let mut handle = handle_mutex.lock().await;
        let turn = handle
            .begin_turn(&req.message)
            .map_err(|_| ServerError::AgentBusy(agent_id.clone()))?;

        let cancel = tokio_util::sync::CancellationToken::new();
        {
            let mut tokens = state.cancel_tokens.write().await;
            tokens.insert(agent_id.clone(), cancel);
        }

        (turn, handle.factory.clone())
    };

    // Create a per-request event bus for this streaming session
    let event_bus = Arc::new(SessionEventBus::new(state.config.replay_capacity));
    {
        let mut buses = state.event_buses.write().await;
        buses.insert(agent_id.clone(), Arc::clone(&event_bus));
    }

    // Build agent for this turn
    let agent = factory
        .build_agent(
            Arc::new(ServerHook::new()),
            SessionOpts::New {
                session_id: agent_id.clone(),
            },
            vec![],
        )
        .await;

    let (stream_rx, done_rx) = agent
        .run_streaming_with_history(turn_input.messages, turn_input.subturn_resume)
        .await
        .map_err(ServerError::Internal)?;

    // Channel for SSE events sent to the client
    let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<Event>(128);

    // Background task: read StreamChunks, convert to SSE events
    let bus = Arc::clone(&event_bus);
    let agent_id_bg = agent_id.clone();
    let handle_mutex_bg = Arc::clone(&handle_mutex);

    tokio::spawn(async move {
        let mut stream = ReceiverStream::new(stream_rx);
        let mut session_id = None;

        while let Some(chunk) = stream.next().await {
            let (event_type, data) = match &chunk {
                StreamChunk::Delta { text } => {
                    ("delta", serde_json::json!({ "text": text }).to_string())
                }
                StreamChunk::ToolCallReady { call } => (
                    "tool_call",
                    serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                        "args": call.args
                    })
                    .to_string(),
                ),
                StreamChunk::Done { usage } => (
                    "usage",
                    serde_json::json!({
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens
                    })
                    .to_string(),
                ),
                StreamChunk::Status { text } => {
                    ("status", serde_json::json!({ "text": text }).to_string())
                }
            };

            let id = bus.publish(event_type, data.clone()).await;

            let event = Event::default()
                .event(event_type)
                .data(data)
                .id(id.to_string());

            if sse_tx.send(event).await.is_err() {
                break;
            }
        }

        // Wait for the done signal with final messages
        if let Ok(Ok((_sid, final_messages))) = done_rx.await {
            session_id = _sid;

            // Update handle via complete_turn
            let mut handle = handle_mutex_bg.lock().await;
            handle.complete_turn(final_messages);
        } else {
            // Error or cancelled — abort turn
            let mut handle = handle_mutex_bg.lock().await;
            handle.abort_turn();
        }

        // Send done event
        let done_data = serde_json::json!({
            "session_id": session_id,
            "agent_id": agent_id_bg,
        })
        .to_string();
        let id = bus.publish("done", done_data.clone()).await;
        let _ = sse_tx
            .send(
                Event::default()
                    .event("done")
                    .data(done_data)
                    .id(id.to_string()),
            )
            .await;
    });

    let stream = ReceiverStream::new(sse_rx).map(Ok);

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new().interval(std::time::Duration::from_millis(state.config.heartbeat_ms)),
    ))
}

#[derive(Debug, serde::Deserialize)]
pub struct ReconnectQuery {
    pub last_event_id: Option<u64>,
}

/// Reconnect to an agent's SSE event stream.
///
/// Returns buffered events since `last_event_id`, then switches to live stream.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{agent_id}/chat/events",
    params(
        ("agent_id" = String, Path, description = "Agent ID"),
        ("last_event_id" = Option<u64>, Query, description = "Last received event ID for replay"),
    ),
    responses(
        (status = 200, description = "SSE event stream"),
        (status = 404, description = "Agent not found or no active stream"),
    ),
    tag = "chat"
)]
pub async fn events(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    Query(query): Query<ReconnectQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ServerError> {
    // Get the event bus for this agent
    let bus = {
        let buses = state.event_buses.read().await;
        buses
            .get(&agent_id)
            .cloned()
            .ok_or_else(|| ServerError::AgentNotFound(agent_id.clone()))?
    };

    let (replay, mut live_rx) = bus.subscribe(query.last_event_id).await;

    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(128);

    tokio::spawn(async move {
        // Send replayed events first
        for event in replay {
            let sse = Event::default()
                .event(&event.event_type)
                .data(&event.data)
                .id(event.id.to_string());
            if tx.send(sse).await.is_err() {
                return;
            }
        }

        // Then stream live events
        loop {
            match live_rx.recv().await {
                Ok(event) => {
                    let sse = Event::default()
                        .event(&event.event_type)
                        .data(&event.data)
                        .id(event.id.to_string());
                    if tx.send(sse).await.is_err() {
                        return;
                    }
                    if event.event_type == "done" {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    let err = Event::default().event("error").data(
                        serde_json::json!({
                            "message": format!("Lagged behind by {n} events")
                        })
                        .to_string(),
                    );
                    let _ = tx.send(err).await;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    let stream = ReceiverStream::new(rx).map(Ok);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new().interval(std::time::Duration::from_millis(state.config.heartbeat_ms)),
    ))
}

/// Cancel an in-flight chat request.
#[utoipa::path(
    delete,
    path = "/api/v1/agents/{agent_id}/chat",
    params(
        ("agent_id" = String, Path, description = "Agent ID")
    ),
    responses(
        (status = 204, description = "Chat cancelled"),
        (status = 404, description = "Agent not found"),
    ),
    tag = "chat"
)]
pub async fn cancel_chat(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
) -> Result<axum::http::StatusCode, ServerError> {
    let tokens = state.cancel_tokens.read().await;
    if let Some(cancel) = tokens.get(&agent_id) {
        cancel.cancel();
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/agents/{agent_id}/chat",
            post(chat).delete(cancel_chat),
        )
        .route("/api/v1/agents/{agent_id}/chat/events", get(events))
}
