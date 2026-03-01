use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

/// The event payload passed to every hook callback.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Fired once before the first LLM call.
    AgentStart { task: String },
    /// Fired once after the agent produces its final response.
    AgentStop { result: String },
    /// Fired at the top of each agent turn (before the LLM call).
    TurnStart { turn: usize },
    /// Fired at the bottom of each agent turn (after all tool calls are done).
    TurnEnd { turn: usize },
    /// Fired before a tool executes. Hooks may block or modify the call.
    PreToolUse {
        tool_name: String,
        args: Value,
        tool_use_id: String,
    },
    /// Fired after a tool succeeds.
    PostToolUse {
        tool_name: String,
        args: Value,
        result: String,
        tool_use_id: String,
    },
    /// Fired after a tool returns an error.
    PostToolUseFailure {
        tool_name: String,
        args: Value,
        error: String,
        tool_use_id: String,
    },
}

impl HookEvent {
    /// Returns the tool name if this is a tool-related event.
    pub fn tool_name(&self) -> Option<&str> {
        match self {
            Self::PreToolUse { tool_name, .. }
            | Self::PostToolUse { tool_name, .. }
            | Self::PostToolUseFailure { tool_name, .. } => Some(tool_name),
            _ => None,
        }
    }
}

/// Decision returned by a `PreToolUse` hook.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolUseDecision {
    Allow,
    Deny {
        reason: String,
    },
    /// Replace the tool arguments before execution.
    ModifyArgs {
        args: Value,
    },
}

/// What a hook callback returns to influence agent behaviour.
#[derive(Debug, Clone, Default)]
pub enum HookOutput {
    /// No opinion — let the agent proceed normally.
    #[default]
    Continue,
    /// PreToolUse only: control whether/how the tool executes.
    ToolDecision(ToolUseDecision),
    /// PostToolUse only: append text to the tool result visible to the LLM.
    AppendContext(String),
    /// Inject an extra system message into the conversation this turn.
    SystemMessage(String),
    /// Halt the agent after this hook fires.
    Stop,
}

/// A hook that intercepts agent lifecycle events.
///
/// Register hooks via `KrabsAgentBuilder::hook()`. Multiple hooks are executed
/// in registration order; for `PreToolUse`, `Deny` takes priority over
/// `ModifyArgs`, which takes priority over `Allow`.
#[async_trait]
pub trait Hook: Send + Sync {
    /// Optional regex pattern matched against the tool name for tool events.
    /// `None` means the hook runs for every occurrence of the event.
    fn matcher(&self) -> Option<&str> {
        None
    }

    async fn on_event(&self, event: &HookEvent) -> Result<HookOutput>;
}
