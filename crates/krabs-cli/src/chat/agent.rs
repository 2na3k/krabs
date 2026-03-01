use std::sync::Arc;

use krabs_core::{LlmProvider, Message, StreamChunk, ToolRegistry, UserInputRequest};
use tokio::sync::{mpsc, oneshot};

use super::app::extract_api_error;
use super::types::DisplayEvent;

// ── TUI hook — bridges KrabsAgent lifecycle events into DisplayEvents ─────────

struct TuiHook {
    tx: mpsc::Sender<DisplayEvent>,
}

#[async_trait::async_trait]
impl krabs_core::Hook for TuiHook {
    async fn on_event(
        &self,
        event: &krabs_core::HookEvent,
    ) -> anyhow::Result<krabs_core::HookOutput> {
        use krabs_core::{HookEvent, HookOutput, ToolUseDecision};
        match event {
            // Before a tool runs: ask the user for permission
            HookEvent::PreToolUse {
                tool_name,
                args,
                tool_use_id: _,
            } => {
                let (respond, rx) = oneshot::channel::<bool>();
                let args_str = serde_json::to_string(args).unwrap_or_default();
                // If the send fails the channel is closed (turn cancelled) — deny
                if self
                    .tx
                    .send(DisplayEvent::PermissionRequest {
                        tool_name: tool_name.clone(),
                        args: args_str,
                        respond,
                    })
                    .await
                    .is_err()
                {
                    return Ok(HookOutput::ToolDecision(ToolUseDecision::Deny {
                        reason: "channel closed".into(),
                    }));
                }
                let allowed = rx.await.unwrap_or(false);
                if allowed {
                    Ok(HookOutput::Continue)
                } else {
                    Ok(HookOutput::ToolDecision(ToolUseDecision::Deny {
                        reason: "denied by user".into(),
                    }))
                }
            }
            // After a tool succeeds: show the result in the TUI
            HookEvent::PostToolUse { result, .. } => {
                let _ = self
                    .tx
                    .send(DisplayEvent::ToolResultEnd(result.clone()))
                    .await;
                Ok(HookOutput::Continue)
            }
            _ => Ok(HookOutput::Continue),
        }
    }
}

// ── background agentic task ──────────────────────────────────────────────────

/// Build a per-turn `KrabsAgent` with the given provider, registry, system
/// prompt, and a `TuiHook` wired to the display-event channel.
pub(super) async fn build_agent(
    config: &krabs_core::KrabsConfig,
    provider: Arc<dyn LlmProvider>,
    registry: Arc<ToolRegistry>,
    system_prompt: String,
    tx: mpsc::Sender<DisplayEvent>,
    resume_session_id: Option<String>,
) -> Arc<krabs_core::KrabsAgent> {
    use krabs_core::{DelegateTool, DispatchTool, UserInputTool};

    let mut tool_registry = ToolRegistry::new();
    for name in registry.names() {
        if let Some(t) = registry.get(&name) {
            tool_registry.register(t);
        }
    }
    // Register orchestration tools so the agent can spawn specialised sub-agents.
    tool_registry.register(Arc::new(DelegateTool::new(
        config.clone(),
        Arc::clone(&provider),
        tool_registry.clone(),
        krabs_core::PermissionGuard::new(),
    )));
    tool_registry.register(Arc::new(DispatchTool::new(
        config.clone(),
        Arc::clone(&provider),
        tool_registry.clone(),
        krabs_core::PermissionGuard::new(),
    )));
    // Register the ask_user tool: a dedicated channel forwards requests to the
    // TUI event loop as DisplayEvent::UserInput, blocking the agent until the
    // user confirms their choice in the popup.
    let (ui_tx, mut ui_rx) = mpsc::channel::<UserInputRequest>(4);
    let fwd_tx = tx.clone();
    tokio::spawn(async move {
        while let Some(req) = ui_rx.recv().await {
            let _ = fwd_tx.send(DisplayEvent::UserInput(req)).await;
        }
    });
    tool_registry.register(Arc::new(UserInputTool::new(ui_tx)));
    let builder = krabs_core::KrabsAgentBuilder::new(config.clone(), provider)
        .registry(tool_registry)
        .system_prompt(system_prompt)
        .hook(Arc::new(TuiHook { tx }));
    let builder = match resume_session_id {
        Some(sid) => builder.resume_session(sid),
        None => builder,
    };
    builder.build_async().await
}

pub(super) async fn run_agent_turn(
    agent: Arc<krabs_core::KrabsAgent>,
    messages: Vec<Message>,
    tx: mpsc::Sender<DisplayEvent>,
) {
    let session_id = agent.session_id().map(|s| s.to_string());
    let (mut stream, done_rx) = match agent.run_streaming_with_history(messages).await {
        Ok(r) => r,
        Err(e) => {
            let _ = tx
                .send(DisplayEvent::Error {
                    message: extract_api_error(&e.to_string()),
                    session_id,
                })
                .await;
            return;
        }
    };

    while let Some(chunk) = stream.recv().await {
        match chunk {
            StreamChunk::Delta { text } => {
                if tx.send(DisplayEvent::Token(text)).await.is_err() {
                    return;
                }
            }
            StreamChunk::ToolCallReady { call } => {
                if tx.send(DisplayEvent::ToolCallStart(call)).await.is_err() {
                    return;
                }
            }
            StreamChunk::Done { usage } => {
                if tx.send(DisplayEvent::TurnUsage(usage)).await.is_err() {
                    return;
                }
            }
            StreamChunk::Status { text } => {
                if tx.send(DisplayEvent::Status(text)).await.is_err() {
                    return;
                }
            }
        }
    }

    // Stream closed — get final message history from done channel
    let (session_id, final_messages) = match done_rx.await {
        Ok(Ok((sid, msgs))) => (sid, msgs),
        Ok(Err(e)) => {
            let _ = tx
                .send(DisplayEvent::Error {
                    message: extract_api_error(&e.to_string()),
                    session_id,
                })
                .await;
            return;
        }
        Err(_) => {
            // sender dropped without sending — treat as empty (turn was cancelled)
            return;
        }
    };
    let _ = tx.send(DisplayEvent::Done { messages: final_messages, session_id }).await;
}
