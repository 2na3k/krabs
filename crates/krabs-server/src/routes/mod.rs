pub mod agents;
pub mod chat;
pub mod config_api;
pub mod health;
pub mod history;
pub mod openapi;
pub mod sessions;
pub mod tools;

use axum::middleware;
use axum::Router;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::auth::auth_middleware;
use crate::state::AppState;

/// Assemble the full router from all route modules.
pub fn router(state: Arc<AppState>) -> Router {
    let cors = if state.config.cors_origins.is_empty() {
        CorsLayer::permissive()
    } else {
        let origins: Vec<_> = state
            .config
            .cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(tower_http::cors::Any)
            .allow_headers(tower_http::cors::Any)
    };

    Router::new()
        .merge(health::router())
        .merge(agents::router())
        .merge(chat::router())
        .merge(history::router())
        .merge(sessions::router())
        .merge(tools::router())
        .merge(config_api::router())
        .merge(openapi::router())
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
