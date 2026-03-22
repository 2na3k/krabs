use anyhow::Result;
use async_trait::async_trait;
use krabs_core::{HookEvent, HookOutput};

/// Server-side hook that auto-approves all tool calls.
///
/// Mirrors `TuiHook` from krabs-cli but operates headlessly: no interactive
/// permission prompts. All `PreToolUse` events are approved unconditionally.
pub struct ServerHook;

impl Default for ServerHook {
    fn default() -> Self {
        Self
    }
}

impl ServerHook {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl krabs_core::Hook for ServerHook {
    async fn on_event(&self, _event: &HookEvent) -> Result<HookOutput> {
        // In server mode, auto-approve everything.
        // Tool-start/result/error events are forwarded to the SSE stream
        // by the chat handler reading from StreamChunk, not from hooks.
        Ok(HookOutput::Continue)
    }
}
