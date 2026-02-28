use crate::agents::agent::{Agent, KrabsAgentBuilder};
use crate::agents::base_agent::BaseAgent;
use crate::config::config::KrabsConfig;
use crate::memory::memory::InMemoryStore;
use crate::permissions::PermissionGuard;
use crate::providers::provider::LlmProvider;
use crate::tools::registry::ToolRegistry;
use crate::tools::tool::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// A tool that lets the running agent delegate a task to a specialised sub-agent.
///
/// The sub-agent is built on-demand with the requested `BaseAgent` profile as its
/// system prompt (layered on top of the immutable SOUL + SYSTEM_PROMPT base).
/// It shares the same config, provider, tool registry, and permissions as the parent.
///
/// # JSON schema
/// ```json
/// {
///   "profile": "planner",   // name of a built-in BaseAgent profile
///   "task":    "..."        // the task description to run
/// }
/// ```
pub struct DelegateTool {
    config: KrabsConfig,
    provider: Arc<dyn LlmProvider>,
    registry: ToolRegistry,
    permissions: PermissionGuard,
}

impl DelegateTool {
    pub fn new(
        config: KrabsConfig,
        provider: Arc<dyn LlmProvider>,
        registry: ToolRegistry,
        permissions: PermissionGuard,
    ) -> Self {
        Self {
            config,
            provider,
            registry,
            permissions,
        }
    }

    /// Resolve a profile name to a `BaseAgent` variant.
    fn resolve_profile(name: &str) -> Option<BaseAgent> {
        BaseAgent::all().iter().find(|a| a.name() == name).copied()
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate"
    }

    fn description(&self) -> &str {
        "Delegate a task to a specialised sub-agent. \
         The sub-agent runs the task with a role-specific system prompt and returns its output. \
         Use this to hand off work that belongs to a specific role (e.g. planner, frontend_developer)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "profile": {
                    "type": "string",
                    "description": "The built-in agent profile to use. One of: planner, frontend_developer."
                },
                "task": {
                    "type": "string",
                    "description": "The task for the sub-agent to complete."
                }
            },
            "required": ["profile", "task"]
        })
    }

    async fn call(&self, args: Value) -> Result<ToolResult> {
        let profile_name = args["profile"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: profile"))?;

        let task = args["task"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: task"))?;

        let profile = Self::resolve_profile(profile_name).ok_or_else(|| {
            let available: Vec<&str> = BaseAgent::all().iter().map(|a| a.name()).collect();
            anyhow::anyhow!(
                "unknown profile '{}'. Available profiles: {}",
                profile_name,
                available.join(", ")
            )
        })?;

        let agent = KrabsAgentBuilder::new(self.config.clone(), Arc::clone(&self.provider))
            .registry(self.registry.clone())
            .memory(InMemoryStore::new())
            .permissions(self.permissions.clone())
            .system_prompt(profile.system_prompt())
            .build();

        let output = Agent::run(agent.as_ref(), task).await?;

        Ok(ToolResult {
            content: format!(
                "[{} sub-agent — {} tool call(s)]\n{}",
                profile_name, output.tool_calls_made, output.result
            ),
            is_error: false,
        })
    }
}
