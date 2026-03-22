use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use std::sync::Arc;

use crate::dto::ToolInfo;
use crate::error::ServerError;
use crate::state::AppState;

/// List tools available to a specific agent.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{agent_id}/tools",
    params(
        ("agent_id" = String, Path, description = "Agent ID")
    ),
    responses(
        (status = 200, description = "Tool list", body = Vec<ToolInfo>),
        (status = 404, description = "Agent not found"),
    ),
    tag = "tools"
)]
pub async fn list_tools(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
) -> Result<Json<Vec<ToolInfo>>, ServerError> {
    let handle_mutex = state
        .agent_pool
        .get(&agent_id)
        .await
        .map_err(ServerError::from)?;
    let handle = handle_mutex.lock().await;

    let tools: Vec<ToolInfo> = handle
        .factory
        .registry()
        .tool_defs()
        .into_iter()
        .map(|td| ToolInfo {
            name: td.name,
            description: td.description,
            parameters: td.parameters,
        })
        .collect();

    Ok(Json(tools))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/agents/{agent_id}/tools", get(list_tools))
}
