use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

use crate::error::ServerError;
use crate::state::AppState;

/// Middleware that validates the `X-Secret-Key` header against the configured
/// secret. If no secret is configured, all requests are allowed.
///
/// Exempt paths: `/api/v1/health`, `/openapi.json`.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, ServerError> {
    let Some(ref secret) = state.config.secret_key else {
        return Ok(next.run(request).await);
    };

    // Exempt health and OpenAPI endpoints.
    let path = request.uri().path();
    if path == "/api/v1/health" || path == "/openapi.json" {
        return Ok(next.run(request).await);
    }

    let provided = request
        .headers()
        .get("X-Secret-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Constant-time comparison to prevent timing attacks.
    if constant_time_eq(provided.as_bytes(), secret.as_bytes()) {
        Ok(next.run(request).await)
    } else {
        Err(ServerError::Unauthorized)
    }
}

/// Constant-time byte comparison (no external dependency).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
