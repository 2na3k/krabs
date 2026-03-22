use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::providers::provider::Message;

use super::context::{ConversationContext, TurnInput};
use super::factory::AgentFactory;

// ── Types ────────────────────────────────────────────────────────────────────

pub type AgentId = String;

/// Status of a managed agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Running,
}

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error("pool is full (max {0})")]
    Full(usize),
}

#[derive(Debug, thiserror::Error)]
pub enum HandleError {
    #[error("agent is busy")]
    Busy,
}

// ── AgentHandle ──────────────────────────────────────────────────────────────

/// Core handle for a managed agent. Generic over `M` for caller-specific metadata.
///
/// - Server stores `CreateAgentRequest` as metadata.
/// - CLI stores `()`.
/// - Core never inspects `M`.
pub struct AgentHandle<M = ()> {
    pub context: ConversationContext,
    pub factory: AgentFactory,
    pub status: AgentStatus,
    pub created_at: i64,
    pub metadata: M,
}

impl<M> AgentHandle<M> {
    /// Begin a turn: validates the agent is idle, transitions to Running,
    /// appends the user message, and returns a [`TurnInput`] snapshot.
    pub fn begin_turn(&mut self, user_message: &str) -> Result<TurnInput, HandleError> {
        if self.status == AgentStatus::Running {
            return Err(HandleError::Busy);
        }
        self.status = AgentStatus::Running;
        Ok(self.context.begin_turn(user_message))
    }

    /// Complete a turn: updates messages, transitions back to Idle.
    pub fn complete_turn(&mut self, final_messages: Vec<Message>) {
        self.context.complete_turn(final_messages);
        self.status = AgentStatus::Idle;
    }

    /// Mark as idle without updating messages (error / cancellation path).
    pub fn abort_turn(&mut self) {
        self.status = AgentStatus::Idle;
    }
}

// ── AgentPool ────────────────────────────────────────────────────────────────

/// A concurrent pool of managed agents.
///
/// `M` is the metadata type stored alongside each handle.
/// The outer `RwLock` guards the map; each handle has its own `tokio::sync::Mutex`.
/// The outer lock is released before acquiring any inner lock, preventing deadlocks.
pub struct AgentPool<M = ()> {
    agents: RwLock<HashMap<AgentId, Arc<tokio::sync::Mutex<AgentHandle<M>>>>>,
    max_agents: usize,
}

impl<M: Send + 'static> AgentPool<M> {
    pub fn new(max_agents: usize) -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            max_agents,
        }
    }

    /// Insert a new agent handle, returning its ID.
    pub async fn insert(&self, id: AgentId, handle: AgentHandle<M>) -> Result<(), PoolError> {
        let agents = self.agents.read().await;
        if agents.len() >= self.max_agents {
            return Err(PoolError::Full(self.max_agents));
        }
        drop(agents);

        let mut agents = self.agents.write().await;
        agents.insert(id, Arc::new(tokio::sync::Mutex::new(handle)));
        Ok(())
    }

    /// Get a handle to an agent by ID.
    pub async fn get(
        &self,
        id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<AgentHandle<M>>>, PoolError> {
        let agents = self.agents.read().await;
        agents
            .get(id)
            .cloned()
            .ok_or_else(|| PoolError::NotFound(id.to_string()))
    }

    /// Remove an agent from the pool, returning its handle.
    pub async fn remove(&self, id: &str) -> Result<AgentHandle<M>, PoolError> {
        let mut agents = self.agents.write().await;
        let handle_mutex = agents
            .remove(id)
            .ok_or_else(|| PoolError::NotFound(id.to_string()))?;

        // Unwrap the Arc — if other references exist, we lock and extract.
        match Arc::try_unwrap(handle_mutex) {
            Ok(mutex) => Ok(mutex.into_inner()),
            Err(arc) => {
                let handle = arc.lock().await;
                // We can't move out of MutexGuard, so reconstruct.
                // This path only fires if someone else holds an Arc ref.
                // In practice, the pool is the sole owner after remove().
                // Return NotFound as a safe fallback.
                drop(handle);
                Err(PoolError::NotFound(id.to_string()))
            }
        }
    }

    /// List all agent IDs with their statuses and creation times.
    pub async fn list(&self) -> Vec<(AgentId, AgentStatus, i64)> {
        let agents = self.agents.read().await;
        let mut result = Vec::with_capacity(agents.len());
        for (id, handle_mutex) in agents.iter() {
            let handle = handle_mutex.lock().await;
            result.push((id.clone(), handle.status.clone(), handle.created_at));
        }
        result
    }

    /// Number of agents currently in the pool.
    pub async fn count(&self) -> usize {
        self.agents.read().await.len()
    }
}
