use crate::hooks::hook::{Hook, HookEvent, HookOutput, ToolUseDecision};
use regex::Regex;
use std::sync::Arc;
use tracing::warn;

pub struct HookRegistry {
    hooks: Vec<Arc<dyn Hook>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    pub fn register(&mut self, hook: Arc<dyn Hook>) {
        self.hooks.push(hook);
    }

    /// Fire all matching hooks for an event and return the resolved output.
    ///
    /// Resolution rules:
    /// - `PreToolUse`: Deny > ModifyArgs > Allow (first match wins per tier)
    /// - All other events: first Stop > first SystemMessage > first AppendContext > Continue
    pub async fn fire(&self, event: &HookEvent) -> HookOutput {
        let matching: Vec<_> = self
            .hooks
            .iter()
            .filter(|h| self.matches(h.as_ref(), event))
            .collect();

        if matching.is_empty() {
            return HookOutput::Continue;
        }

        let mut outputs = Vec::with_capacity(matching.len());
        for hook in matching {
            match hook.on_event(event).await {
                Ok(out) => outputs.push(out),
                Err(e) => warn!("hook error on {:?}: {}", event.tool_name(), e),
            }
        }

        if matches!(event, HookEvent::PreToolUse { .. }) {
            resolve_pre_tool_use(outputs)
        } else {
            resolve_general(outputs)
        }
    }

    fn matches(&self, hook: &dyn Hook, event: &HookEvent) -> bool {
        let Some(pattern) = hook.matcher() else {
            return true;
        };
        let Some(tool_name) = event.tool_name() else {
            // Non-tool events: matcher is ignored, hook runs
            return true;
        };
        match Regex::new(pattern) {
            Ok(re) => re.is_match(tool_name),
            Err(e) => {
                warn!("invalid hook matcher pattern '{}': {}", pattern, e);
                false
            }
        }
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Deny > ModifyArgs > Allow
fn resolve_pre_tool_use(outputs: Vec<HookOutput>) -> HookOutput {
    let mut modify = None;

    for out in outputs {
        match out {
            HookOutput::ToolDecision(ToolUseDecision::Deny { .. }) => return out,
            HookOutput::ToolDecision(ToolUseDecision::ModifyArgs { .. }) if modify.is_none() => {
                modify = Some(out);
            }
            _ => {}
        }
    }

    modify.unwrap_or(HookOutput::Continue)
}

/// Stop > SystemMessage > AppendContext > Continue
fn resolve_general(outputs: Vec<HookOutput>) -> HookOutput {
    let mut system_msg = None;
    let mut append_ctx = None;

    for out in outputs {
        match out {
            HookOutput::Stop => return HookOutput::Stop,
            HookOutput::SystemMessage(_) if system_msg.is_none() => system_msg = Some(out),
            HookOutput::AppendContext(_) if append_ctx.is_none() => append_ctx = Some(out),
            _ => {}
        }
    }

    system_msg.or(append_ctx).unwrap_or(HookOutput::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::hook::{Hook, HookEvent, HookOutput, ToolUseDecision};
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn pre_tool_event(tool: &str) -> HookEvent {
        HookEvent::PreToolUse {
            tool_name: tool.to_string(),
            args: json!({"cmd": "ls"}),
            tool_use_id: "id-1".to_string(),
        }
    }

    fn post_tool_event(tool: &str) -> HookEvent {
        HookEvent::PostToolUse {
            tool_name: tool.to_string(),
            args: json!({}),
            result: "ok".to_string(),
            tool_use_id: "id-1".to_string(),
        }
    }

    struct FixedHook {
        output: HookOutput,
        matcher: Option<&'static str>,
    }

    impl FixedHook {
        fn new(output: HookOutput) -> Arc<Self> {
            Arc::new(Self {
                output,
                matcher: None,
            })
        }

        fn with_matcher(output: HookOutput, matcher: &'static str) -> Arc<Self> {
            Arc::new(Self {
                output,
                matcher: Some(matcher),
            })
        }
    }

    #[async_trait]
    impl Hook for FixedHook {
        fn matcher(&self) -> Option<&str> {
            self.matcher
        }

        async fn on_event(&self, _event: &HookEvent) -> Result<HookOutput> {
            Ok(self.output.clone())
        }
    }

    struct ErrorHook;

    #[async_trait]
    impl Hook for ErrorHook {
        async fn on_event(&self, _event: &HookEvent) -> Result<HookOutput> {
            anyhow::bail!("hook exploded")
        }
    }

    // ── empty registry ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn empty_registry_returns_continue() {
        let reg = HookRegistry::new();
        let out = reg.fire(&pre_tool_event("bash")).await;
        assert!(matches!(out, HookOutput::Continue));
    }

    // ── PreToolUse resolution: Deny > ModifyArgs > Continue ──────────────────

    #[tokio::test]
    async fn pre_tool_allow_returns_continue() {
        let mut reg = HookRegistry::new();
        reg.register(FixedHook::new(HookOutput::Continue));
        let out = reg.fire(&pre_tool_event("bash")).await;
        assert!(matches!(out, HookOutput::Continue));
    }

    #[tokio::test]
    async fn pre_tool_deny_wins_over_modify() {
        let mut reg = HookRegistry::new();
        reg.register(FixedHook::new(HookOutput::ToolDecision(
            ToolUseDecision::ModifyArgs {
                args: json!({"cmd": "echo"}),
            },
        )));
        reg.register(FixedHook::new(HookOutput::ToolDecision(
            ToolUseDecision::Deny {
                reason: "blocked".to_string(),
            },
        )));
        let out = reg.fire(&pre_tool_event("bash")).await;
        assert!(matches!(
            out,
            HookOutput::ToolDecision(ToolUseDecision::Deny { .. })
        ));
    }

    #[tokio::test]
    async fn pre_tool_modify_wins_over_continue() {
        let new_args = json!({"cmd": "echo hello"});
        let mut reg = HookRegistry::new();
        reg.register(FixedHook::new(HookOutput::Continue));
        reg.register(FixedHook::new(HookOutput::ToolDecision(
            ToolUseDecision::ModifyArgs {
                args: new_args.clone(),
            },
        )));
        let out = reg.fire(&pre_tool_event("bash")).await;
        match out {
            HookOutput::ToolDecision(ToolUseDecision::ModifyArgs { args }) => {
                assert_eq!(args, new_args);
            }
            other => panic!("expected ModifyArgs, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn pre_tool_first_modify_wins_when_multiple() {
        let first_args = json!({"cmd": "first"});
        let second_args = json!({"cmd": "second"});
        let mut reg = HookRegistry::new();
        reg.register(FixedHook::new(HookOutput::ToolDecision(
            ToolUseDecision::ModifyArgs {
                args: first_args.clone(),
            },
        )));
        reg.register(FixedHook::new(HookOutput::ToolDecision(
            ToolUseDecision::ModifyArgs { args: second_args },
        )));
        let out = reg.fire(&pre_tool_event("bash")).await;
        match out {
            HookOutput::ToolDecision(ToolUseDecision::ModifyArgs { args }) => {
                assert_eq!(args, first_args);
            }
            other => panic!("expected first ModifyArgs, got {:?}", other),
        }
    }

    // ── general resolution: Stop > SystemMessage > AppendContext > Continue ──

    #[tokio::test]
    async fn general_stop_wins() {
        let mut reg = HookRegistry::new();
        reg.register(FixedHook::new(HookOutput::SystemMessage("msg".into())));
        reg.register(FixedHook::new(HookOutput::Stop));
        let out = reg.fire(&post_tool_event("bash")).await;
        assert!(matches!(out, HookOutput::Stop));
    }

    #[tokio::test]
    async fn general_system_message_wins_over_append() {
        let mut reg = HookRegistry::new();
        reg.register(FixedHook::new(HookOutput::AppendContext("ctx".into())));
        reg.register(FixedHook::new(HookOutput::SystemMessage("sys".into())));
        let out = reg.fire(&post_tool_event("bash")).await;
        assert!(matches!(out, HookOutput::SystemMessage(_)));
    }

    #[tokio::test]
    async fn general_append_context_returned_when_no_stop_or_system() {
        let mut reg = HookRegistry::new();
        reg.register(FixedHook::new(HookOutput::AppendContext("extra".into())));
        let out = reg.fire(&post_tool_event("bash")).await;
        match out {
            HookOutput::AppendContext(s) => assert_eq!(s, "extra"),
            other => panic!("expected AppendContext, got {:?}", other),
        }
    }

    // ── matcher filtering ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn matcher_filters_by_tool_name() {
        let mut reg = HookRegistry::new();
        // Only fires for "write" tools
        reg.register(FixedHook::with_matcher(
            HookOutput::ToolDecision(ToolUseDecision::Deny {
                reason: "no writes".into(),
            }),
            "write",
        ));
        // bash does not match — expect Continue
        let out = reg.fire(&pre_tool_event("bash")).await;
        assert!(matches!(out, HookOutput::Continue));

        // write matches — expect Deny
        let out = reg.fire(&pre_tool_event("write")).await;
        assert!(matches!(
            out,
            HookOutput::ToolDecision(ToolUseDecision::Deny { .. })
        ));
    }

    #[tokio::test]
    async fn matcher_regex_matches_multiple_tools() {
        let mut reg = HookRegistry::new();
        reg.register(FixedHook::with_matcher(
            HookOutput::ToolDecision(ToolUseDecision::Deny {
                reason: "blocked".into(),
            }),
            "write|edit",
        ));
        assert!(matches!(
            reg.fire(&pre_tool_event("write")).await,
            HookOutput::ToolDecision(ToolUseDecision::Deny { .. })
        ));
        assert!(matches!(
            reg.fire(&pre_tool_event("edit")).await,
            HookOutput::ToolDecision(ToolUseDecision::Deny { .. })
        ));
        assert!(matches!(
            reg.fire(&pre_tool_event("read")).await,
            HookOutput::Continue
        ));
    }

    #[tokio::test]
    async fn matcher_ignored_for_non_tool_events() {
        // A hook with a matcher should still fire for non-tool events
        let mut reg = HookRegistry::new();
        reg.register(FixedHook::with_matcher(HookOutput::Stop, "bash"));
        let out = reg.fire(&HookEvent::TurnStart { turn: 0 }).await;
        assert!(matches!(out, HookOutput::Stop));
    }

    // ── error handling ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn erroring_hook_is_skipped_not_fatal() {
        let mut reg = HookRegistry::new();
        reg.register(Arc::new(ErrorHook));
        // Should not panic; error is logged and Continue returned
        let out = reg.fire(&pre_tool_event("bash")).await;
        assert!(matches!(out, HookOutput::Continue));
    }

    #[tokio::test]
    async fn erroring_hook_does_not_block_other_hooks() {
        let mut reg = HookRegistry::new();
        reg.register(Arc::new(ErrorHook));
        reg.register(FixedHook::new(HookOutput::ToolDecision(
            ToolUseDecision::Deny {
                reason: "blocked".into(),
            },
        )));
        let out = reg.fire(&pre_tool_event("bash")).await;
        assert!(matches!(
            out,
            HookOutput::ToolDecision(ToolUseDecision::Deny { .. })
        ));
    }

    // ── HookEvent::tool_name ─────────────────────────────────────────────────

    #[test]
    fn hook_event_tool_name_for_tool_events() {
        assert_eq!(pre_tool_event("bash").tool_name(), Some("bash"));
        assert_eq!(post_tool_event("read").tool_name(), Some("read"));
        assert_eq!(
            HookEvent::PostToolUseFailure {
                tool_name: "glob".into(),
                args: json!({}),
                error: "err".into(),
                tool_use_id: "x".into(),
            }
            .tool_name(),
            Some("glob")
        );
    }

    #[test]
    fn hook_event_tool_name_none_for_lifecycle_events() {
        assert!(HookEvent::AgentStart { task: "t".into() }
            .tool_name()
            .is_none());
        assert!(HookEvent::AgentStop { result: "r".into() }
            .tool_name()
            .is_none());
        assert!(HookEvent::TurnStart { turn: 0 }.tool_name().is_none());
        assert!(HookEvent::TurnEnd { turn: 0 }.tool_name().is_none());
    }
}
