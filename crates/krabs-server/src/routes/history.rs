use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use std::sync::Arc;

use crate::dto::{HistoryResponse, MessageDto, ToolCallDto};
use crate::error::ServerError;
use crate::state::AppState;

/// Get the conversation history for an agent.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{agent_id}/history",
    params(
        ("agent_id" = String, Path, description = "Agent ID")
    ),
    responses(
        (status = 200, description = "Conversation history", body = HistoryResponse),
        (status = 404, description = "Agent not found"),
    ),
    tag = "chat"
)]
pub async fn get_history(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
) -> Result<Json<HistoryResponse>, ServerError> {
    let handle_mutex = state
        .agent_pool
        .get(&agent_id)
        .await
        .map_err(ServerError::from)?;
    let handle = handle_mutex.lock().await;

    let messages: Vec<MessageDto> = handle
        .context
        .messages()
        .iter()
        .map(|m| {
            let role = match m.role {
                krabs_core::Role::System => "system",
                krabs_core::Role::User => "user",
                krabs_core::Role::Assistant => "assistant",
                krabs_core::Role::Tool => "tool",
            };

            let tool_calls = m.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|c| ToolCallDto {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        args: c.args.clone(),
                    })
                    .collect()
            });

            MessageDto {
                role: role.to_string(),
                content: m.content.clone(),
                tool_call_id: m.tool_call_id.clone(),
                tool_name: m.tool_name.clone(),
                tool_calls,
            }
        })
        .collect();

    Ok(Json(HistoryResponse {
        agent_id,
        session_id: None,
        messages,
    }))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/agents/{agent_id}/history", get(get_history))
}
