use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;

use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct ServerConfigResponse {
    pub bind: String,
    pub max_agents: usize,
    pub heartbeat_ms: u64,
    pub replay_capacity: usize,
    pub model: String,
    pub base_url: String,
    /// Whether a secret key is configured (value is never exposed).
    pub auth_enabled: bool,
    pub cors_origins: Vec<String>,
}

/// Get current server configuration (secrets masked).
#[utoipa::path(
    get,
    path = "/api/v1/config",
    responses(
        (status = 200, description = "Current server config", body = ServerConfigResponse),
    ),
    tag = "config"
)]
pub async fn get_config(State(state): State<Arc<AppState>>) -> Json<ServerConfigResponse> {
    Json(ServerConfigResponse {
        bind: state.config.bind.clone(),
        max_agents: state.config.max_agents,
        heartbeat_ms: state.config.heartbeat_ms,
        replay_capacity: state.config.replay_capacity,
        model: state.config.krabs.model.clone(),
        base_url: state.config.krabs.base_url.clone(),
        auth_enabled: state.config.secret_key.is_some(),
        cors_origins: state.config.cors_origins.clone(),
    })
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/config", get(get_config))
}
