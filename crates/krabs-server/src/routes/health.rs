use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use std::sync::Arc;

use crate::dto::HealthResponse;
use crate::state::AppState;

/// Health check endpoint.
#[utoipa::path(
    get,
    path = "/api/v1/health",
    responses(
        (status = 200, description = "Server is healthy", body = HealthResponse)
    ),
    tag = "health"
)]
pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: state.start_time.elapsed().as_secs(),
        active_agents: state.agent_pool.count().await,
    })
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/health", get(health))
}
