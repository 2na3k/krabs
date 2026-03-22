use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;

use crate::error::ServerError;
use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionInfo {
    pub id: String,
    pub agent_id: String,
    pub model: String,
    pub provider: String,
    pub created_at: i64,
    pub message_count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionListResponse {
    pub sessions: Vec<SessionInfo>,
}

/// List all persisted sessions.
#[utoipa::path(
    get,
    path = "/api/v1/sessions",
    responses(
        (status = 200, description = "List of sessions", body = SessionListResponse),
    ),
    tag = "sessions"
)]
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SessionListResponse>, ServerError> {
    let store = krabs_core::SessionStore::open(&state.config.krabs.db_path)
        .await
        .map_err(ServerError::Internal)?;

    let summaries = store.list_sessions().await.map_err(ServerError::Internal)?;

    let mut sessions = Vec::with_capacity(summaries.len());
    for s in summaries {
        let count = store.session_message_count(&s.id).await.unwrap_or(0);
        sessions.push(SessionInfo {
            id: s.id,
            agent_id: s.agent_id,
            model: s.model,
            provider: s.provider,
            created_at: s.created_at,
            message_count: count,
        });
    }

    Ok(Json(SessionListResponse { sessions }))
}

/// Get a single session's details.
#[utoipa::path(
    get,
    path = "/api/v1/sessions/{session_id}",
    params(
        ("session_id" = String, Path, description = "Session ID")
    ),
    responses(
        (status = 200, description = "Session details", body = SessionInfo),
        (status = 404, description = "Session not found"),
    ),
    tag = "sessions"
)]
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionInfo>, ServerError> {
    let store = krabs_core::SessionStore::open(&state.config.krabs.db_path)
        .await
        .map_err(ServerError::Internal)?;

    let summaries = store.list_sessions().await.map_err(ServerError::Internal)?;

    let summary = summaries
        .into_iter()
        .find(|s| s.id == session_id)
        .ok_or_else(|| ServerError::SessionNotFound(session_id.clone()))?;

    let count = store.session_message_count(&summary.id).await.unwrap_or(0);

    Ok(Json(SessionInfo {
        id: summary.id,
        agent_id: summary.agent_id,
        model: summary.model,
        provider: summary.provider,
        created_at: summary.created_at,
        message_count: count,
    }))
}

/// Delete a session and all its data.
#[utoipa::path(
    delete,
    path = "/api/v1/sessions/{session_id}",
    params(
        ("session_id" = String, Path, description = "Session ID")
    ),
    responses(
        (status = 204, description = "Session deleted"),
        (status = 404, description = "Session not found"),
    ),
    tag = "sessions"
)]
pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<axum::http::StatusCode, ServerError> {
    let store = krabs_core::SessionStore::open(&state.config.krabs.db_path)
        .await
        .map_err(ServerError::Internal)?;

    store
        .delete_session(&session_id)
        .await
        .map_err(ServerError::Internal)?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/sessions", get(list_sessions))
        .route(
            "/api/v1/sessions/{session_id}",
            get(get_session).delete(delete_session),
        )
}
