use krabs_core::AgentStatus;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ── Health ───────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_secs: u64,
    pub active_agents: usize,
}

// ── Agents ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateAgentRequest {
    /// Human-readable name for this agent.
    pub name: Option<String>,
    /// LLM model override (e.g. "claude-opus-4-6", "gpt-4o").
    pub model: Option<String>,
    /// Provider override ("anthropic", "openai", "gemini").
    pub provider: Option<String>,
    /// Base URL override.
    pub base_url: Option<String>,
    /// API key override.
    pub api_key: Option<String>,
    /// System prompt override.
    pub system_prompt: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateAgentResponse {
    pub agent_id: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AgentInfo {
    pub agent_id: String,
    pub name: Option<String>,
    #[schema(value_type = String)]
    pub status: AgentStatus,
    pub session_id: Option<String>,
    pub model: String,
    pub created_at: i64,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AgentListResponse {
    pub agents: Vec<AgentInfo>,
}

// ── Chat ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChatRequest {
    /// The user message to send to the agent.
    pub message: String,
}

// ── Tools ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ── Messages / History ───────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MessageDto {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDto>>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ToolCallDto {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HistoryResponse {
    pub agent_id: String,
    pub session_id: Option<String>,
    pub messages: Vec<MessageDto>,
}
