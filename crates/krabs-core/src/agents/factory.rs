use std::sync::Arc;

use crate::config::KrabsConfig;
use crate::hooks::hook::Hook;
use crate::providers::provider::LlmProvider;
use crate::tools::registry::ToolRegistry;
use crate::tools::tool::Tool;

use super::agent::KrabsAgentBuilder;

/// Session wiring options for a single turn's agent.
pub enum SessionOpts {
    /// First turn of a new conversation — assign this session ID.
    New { session_id: String },
    /// Resuming an existing session from SQLite.
    Resume { session_id: String },
    /// Subsequent turn — reuse the same session ID.
    Continue { session_id: String },
    /// No session persistence desired.
    None,
}

/// A recipe for building a `KrabsAgent` on each turn.
///
/// Captures the immutable ingredients (config, provider, base registry)
/// so callers don't re-specify them every turn. The `Hook` is provided
/// per-build because CLI and server use different hooks.
#[derive(Clone)]
pub struct AgentFactory {
    config: KrabsConfig,
    provider: Arc<dyn LlmProvider>,
    base_registry: ToolRegistry,
    system_prompt: String,
}

impl AgentFactory {
    pub fn new(
        config: KrabsConfig,
        provider: Arc<dyn LlmProvider>,
        registry: ToolRegistry,
    ) -> Self {
        Self {
            config,
            provider,
            base_registry: registry,
            system_prompt: String::new(),
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn config(&self) -> &KrabsConfig {
        &self.config
    }

    pub fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.provider
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.base_registry
    }

    /// Build a fresh agent for one turn.
    ///
    /// - `hook`: per-turn hook (TuiHook for CLI, ServerHook for server).
    /// - `session_opts`: controls session persistence for this turn.
    /// - `extra_tools`: additional tools beyond the base registry (e.g. UserInputTool).
    pub async fn build_agent(
        &self,
        hook: Arc<dyn Hook>,
        session_opts: SessionOpts,
        extra_tools: Vec<Arc<dyn Tool>>,
    ) -> Arc<crate::agents::agent::KrabsAgent> {
        let mut registry = self.base_registry.clone();
        for tool in extra_tools {
            registry.register(tool);
        }

        let mut builder = KrabsAgentBuilder::new(self.config.clone(), Arc::clone(&self.provider))
            .registry(registry)
            .hook(hook);

        if !self.system_prompt.is_empty() {
            builder = builder.system_prompt(&self.system_prompt);
        }

        builder = match session_opts {
            SessionOpts::New { session_id } => builder.session_id(session_id),
            SessionOpts::Resume { session_id } => builder.resume_session(session_id),
            SessionOpts::Continue { session_id } => builder.session_id(session_id),
            SessionOpts::None => builder,
        };

        builder.build_async().await
    }
}
