use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Agent is busy (currently processing): {0}")]
    AgentBusy(String),

    #[error("Invalid request: {0}")]
    BadRequest(String),

    #[error("Authentication failed")]
    Unauthorized,

    #[error("Agent pool is full")]
    PoolFull,

    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl From<krabs_core::PoolError> for ServerError {
    fn from(e: krabs_core::PoolError) -> Self {
        match e {
            krabs_core::PoolError::NotFound(id) => Self::AgentNotFound(id),
            krabs_core::PoolError::Full(_) => Self::PoolFull,
        }
    }
}

impl From<krabs_core::HandleError> for ServerError {
    fn from(e: krabs_core::HandleError) -> Self {
        match e {
            krabs_core::HandleError::Busy => Self::AgentBusy("agent is busy".to_string()),
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::AgentNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            Self::SessionNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            Self::AgentBusy(_) => (StatusCode::CONFLICT, self.to_string()),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            Self::PoolFull => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            Self::Internal(e) => {
                tracing::error!("Internal error: {e:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_string(),
                )
            }
        };
        let body = serde_json::json!({ "error": message });
        (status, axum::Json(body)).into_response()
    }
}
