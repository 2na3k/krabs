use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use krabs_core::{
    AgentFactory, AgentHandle, AgentStatus, ConversationContext, Credentials, KrabsConfig,
    LlmProvider, ToolRegistry,
};
use std::sync::Arc;

use crate::dto::{AgentInfo, AgentListResponse, CreateAgentRequest, CreateAgentResponse};
use crate::error::ServerError;
use crate::state::AppState;

/// Create a new agent.
#[utoipa::path(
    post,
    path = "/api/v1/agents",
    request_body = CreateAgentRequest,
    responses(
        (status = 201, description = "Agent created", body = CreateAgentResponse),
        (status = 503, description = "Agent pool is full"),
    ),
    tag = "agents"
)]
pub async fn create_agent(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<(axum::http::StatusCode, Json<CreateAgentResponse>), ServerError> {
    let agent_id = uuid::Uuid::new_v4().to_string();

    // Build KrabsConfig with overrides from the request
    let mut config = KrabsConfig::load().unwrap_or_default();
    if let Some(ref model) = req.model {
        config.model.clone_from(model);
    }
    if let Some(ref base_url) = req.base_url {
        config.base_url.clone_from(base_url);
    }
    if let Some(ref api_key) = req.api_key {
        config.api_key.clone_from(api_key);
    }

    // Build provider
    let provider: Arc<dyn LlmProvider> = {
        let provider_name = req
            .provider
            .as_deref()
            .unwrap_or(if !config.provider.is_empty() {
                config.provider.as_str()
            } else {
                "openai"
            });

        let creds = Credentials {
            provider: provider_name.to_string(),
            api_key: config.api_key.clone(),
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            is_default: false,
        };
        Arc::from(creds.build_provider())
    };

    // Build tool registry with defaults + orchestration
    let mut registry = ToolRegistry::with_defaults();
    registry.with_orchestration(&config, &provider);

    let system_prompt = req.system_prompt.clone().unwrap_or_default();

    let factory = AgentFactory::new(config, provider, registry).with_system_prompt(system_prompt);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let handle = AgentHandle {
        context: ConversationContext::new(),
        factory,
        status: AgentStatus::Idle,
        created_at: now,
        metadata: req,
    };

    state
        .agent_pool
        .insert(agent_id.clone(), handle)
        .await
        .map_err(ServerError::from)?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(CreateAgentResponse { agent_id }),
    ))
}

/// List all agents.
#[utoipa::path(
    get,
    path = "/api/v1/agents",
    responses(
        (status = 200, description = "List of agents", body = AgentListResponse),
    ),
    tag = "agents"
)]
pub async fn list_agents(State(state): State<Arc<AppState>>) -> Json<AgentListResponse> {
    let entries = state.agent_pool.list().await;
    let mut agents = Vec::with_capacity(entries.len());

    for (agent_id, status, created_at) in entries {
        let handle_mutex = match state.agent_pool.get(&agent_id).await {
            Ok(h) => h,
            Err(_) => continue,
        };
        let handle = handle_mutex.lock().await;

        agents.push(AgentInfo {
            agent_id,
            name: handle.metadata.name.clone(),
            status,
            session_id: None,
            model: handle.factory.config().model.clone(),
            created_at,
            total_input_tokens: 0,
            total_output_tokens: 0,
        });
    }

    Json(AgentListResponse { agents })
}

/// Get a single agent's details.
#[utoipa::path(
    get,
    path = "/api/v1/agents/{agent_id}",
    params(
        ("agent_id" = String, Path, description = "Agent ID")
    ),
    responses(
        (status = 200, description = "Agent details", body = AgentInfo),
        (status = 404, description = "Agent not found"),
    ),
    tag = "agents"
)]
pub async fn get_agent(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
) -> Result<Json<AgentInfo>, ServerError> {
    let handle_mutex = state
        .agent_pool
        .get(&agent_id)
        .await
        .map_err(ServerError::from)?;
    let handle = handle_mutex.lock().await;

    Ok(Json(AgentInfo {
        agent_id,
        name: handle.metadata.name.clone(),
        status: handle.status.clone(),
        session_id: None,
        model: handle.factory.config().model.clone(),
        created_at: handle.created_at,
        total_input_tokens: 0,
        total_output_tokens: 0,
    }))
}

/// Stop and remove an agent.
#[utoipa::path(
    delete,
    path = "/api/v1/agents/{agent_id}",
    params(
        ("agent_id" = String, Path, description = "Agent ID")
    ),
    responses(
        (status = 204, description = "Agent stopped"),
        (status = 404, description = "Agent not found"),
    ),
    tag = "agents"
)]
pub async fn stop_agent(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
) -> Result<axum::http::StatusCode, ServerError> {
    // Cancel any in-flight work
    {
        let tokens = state.cancel_tokens.read().await;
        if let Some(cancel) = tokens.get(&agent_id) {
            cancel.cancel();
        }
    }
    {
        let mut tokens = state.cancel_tokens.write().await;
        tokens.remove(&agent_id);
    }

    state
        .agent_pool
        .remove(&agent_id)
        .await
        .map_err(ServerError::from)?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/agents", post(create_agent).get(list_agents))
        .route(
            "/api/v1/agents/{agent_id}",
            get(get_agent).delete(stop_agent),
        )
}
