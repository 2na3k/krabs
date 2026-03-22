use axum::routing::get;
use axum::{Json, Router};
use std::sync::Arc;
use utoipa::OpenApi;

use crate::dto::{
    AgentInfo, AgentListResponse, ChatRequest, CreateAgentRequest, CreateAgentResponse,
    HealthResponse, HistoryResponse, MessageDto, ToolCallDto, ToolInfo,
};
use crate::routes::config_api::ServerConfigResponse;
use crate::routes::sessions::{SessionInfo, SessionListResponse};
use crate::state::AppState;

#[derive(OpenApi)]
#[openapi(
    paths(
        crate::routes::health::health,
        crate::routes::agents::create_agent,
        crate::routes::agents::list_agents,
        crate::routes::agents::get_agent,
        crate::routes::agents::stop_agent,
        crate::routes::chat::chat,
        crate::routes::chat::events,
        crate::routes::chat::cancel_chat,
        crate::routes::history::get_history,
        crate::routes::sessions::list_sessions,
        crate::routes::sessions::get_session,
        crate::routes::sessions::delete_session,
        crate::routes::tools::list_tools,
        crate::routes::config_api::get_config,
    ),
    components(schemas(
        HealthResponse,
        CreateAgentRequest,
        CreateAgentResponse,
        AgentInfo,
        AgentListResponse,
        ChatRequest,
        ToolInfo,
        MessageDto,
        ToolCallDto,
        HistoryResponse,
        SessionInfo,
        SessionListResponse,
        ServerConfigResponse,
    )),
    info(
        title = "Krabs Server API",
        version = "0.1.0",
        description = "HTTP API for the Krabs agentic framework"
    ),
    tags(
        (name = "health", description = "Health check endpoints"),
        (name = "agents", description = "Agent lifecycle management"),
        (name = "chat", description = "Agent chat and streaming"),
        (name = "sessions", description = "Session management"),
        (name = "tools", description = "Tool definitions"),
        (name = "config", description = "Server configuration"),
    )
)]
pub struct ApiDoc;

pub async fn openapi_spec() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/openapi.json", get(openapi_spec))
}
