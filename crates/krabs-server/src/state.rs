use crate::config::ServerConfig;
use crate::dto::CreateAgentRequest;
use crate::event_bus::SessionEventBus;
use krabs_core::AgentPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Shared application state, wrapped in `Arc` and passed to all route handlers.
pub struct AppState {
    pub agent_pool: AgentPool<CreateAgentRequest>,
    pub cancel_tokens: RwLock<HashMap<String, CancellationToken>>,
    pub event_buses: RwLock<HashMap<String, Arc<SessionEventBus>>>,
    pub config: ServerConfig,
    pub start_time: Instant,
}

impl AppState {
    pub fn new(config: ServerConfig) -> Arc<Self> {
        let agent_pool = AgentPool::new(config.max_agents);
        Arc::new(Self {
            agent_pool,
            cancel_tokens: RwLock::new(HashMap::new()),
            event_buses: RwLock::new(HashMap::new()),
            config,
            start_time: Instant::now(),
        })
    }
}
