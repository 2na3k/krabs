use crate::config::KrabsConfig;
use crate::hooks::hook::{HookEvent, HookOutput, ToolUseDecision};
use crate::hooks::registry::HookRegistry;
use crate::mcp::mcp::McpRegistry;
use crate::memory::MemoryStore;
use crate::permissions::PermissionGuard;
use crate::providers::provider::{LlmProvider, LlmResponse, Message, Role, StreamChunk};
use crate::sandbox::{SandboxProxy, SandboxedTool};
use crate::session::session::{Session, SessionStore};
use crate::skills::registry::SkillRegistry;
use crate::tools::read_skill::ReadSkillTool;
use crate::tools::registry::ToolRegistry;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

enum ResumeMode {
    New,
    Resume { session_id: String },
}

#[async_trait]
pub trait Agent: Send + Sync {
    async fn run(&self, task: &str) -> Result<AgentOutput>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub result: String,
    pub tool_calls_made: usize,
}

pub struct KrabsAgent {
    pub agent_id: String,
    pub config: KrabsConfig,
    pub provider: Box<dyn LlmProvider>,
    pub registry: ToolRegistry,
    pub memory: Box<dyn MemoryStore>,
    pub permissions: PermissionGuard,
    pub system_prompt: String,
    pub skills: Option<Arc<SkillRegistry>>,
    pub hooks: HookRegistry,
    /// Active session for this agent. `None` only when the SQLite store could
    /// not be opened (e.g. read-only filesystem). Every message and token-usage
    /// row is persisted here automatically by the agent loop.
    pub session: Option<Arc<Session>>,
    /// Sandbox proxy — kept alive for the lifetime of the agent.
    _sandbox_proxy: Option<SandboxProxy>,
    total_input_tokens: std::sync::atomic::AtomicU32,
    total_output_tokens: std::sync::atomic::AtomicU32,
}

pub struct KrabsAgentBuilder {
    agent_id: String,
    config: KrabsConfig,
    provider: Box<dyn LlmProvider>,
    registry: ToolRegistry,
    memory: Box<dyn MemoryStore>,
    permissions: PermissionGuard,
    system_prompt: String,
    skills: Option<Arc<SkillRegistry>>,
    hooks: HookRegistry,
    mcp_registry: Option<McpRegistry>,
    resume_mode: ResumeMode,
}

impl KrabsAgentBuilder {
    pub fn new(config: KrabsConfig, provider: impl LlmProvider + 'static) -> Self {
        Self {
            agent_id: uuid::Uuid::new_v4().to_string(),
            config,
            provider: Box::new(provider),
            registry: ToolRegistry::default(),
            memory: Box::new(crate::memory::memory::InMemoryStore::new()),
            permissions: PermissionGuard::new(),
            system_prompt: String::new(),
            skills: None,
            hooks: HookRegistry::default(),
            mcp_registry: None,
            resume_mode: ResumeMode::New,
        }
    }

    /// Resume a previously persisted session rather than creating a new one.
    /// All new messages will be appended under the original session ID.
    pub fn resume_session(mut self, session_id: impl Into<String>) -> Self {
        self.resume_mode = ResumeMode::Resume {
            session_id: session_id.into(),
        };
        self
    }

    pub fn registry(mut self, registry: ToolRegistry) -> Self {
        self.registry = registry;
        self
    }

    pub fn memory(mut self, memory: impl MemoryStore + 'static) -> Self {
        self.memory = Box::new(memory);
        self
    }

    pub fn permissions(mut self, permissions: PermissionGuard) -> Self {
        self.permissions = permissions;
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn skills(mut self, registry: Arc<SkillRegistry>) -> Self {
        self.registry
            .register(Arc::new(ReadSkillTool::new(Arc::clone(&registry))));
        self.skills = Some(registry);
        self
    }

    pub fn hook(mut self, hook: Arc<dyn crate::hooks::hook::Hook>) -> Self {
        self.hooks.register(hook);
        self
    }

    pub fn with_mcp_registry(mut self, registry: McpRegistry) -> Self {
        self.mcp_registry = Some(registry);
        self
    }

    /// Build the agent, connecting MCP servers and opening the session store.
    ///
    /// This is the preferred builder path — it enables automatic persistence of
    /// every message and token-usage row into the SQLite database at
    /// `config.db_path`.
    pub async fn build_async(mut self) -> Arc<KrabsAgent> {
        if let Some(mcp) = self.mcp_registry.take() {
            let live = mcp.connect_all().await;
            for tool in live.tools_for_all().await {
                self.registry.register(Arc::from(tool));
            }
        }

        // Start sandbox proxy and register sandboxed tool variants if enabled.
        let sandbox_proxy = if self.config.sandbox.enabled {
            let sandbox_cfg = Arc::new(self.config.sandbox.clone());
            match SandboxProxy::start(Arc::clone(&sandbox_cfg)).await {
                Ok(proxy) => {
                    let port = proxy.port();
                    self.registry.register(Arc::new(SandboxedTool::wrap(
                        crate::tools::bash::BashTool,
                        Arc::clone(&sandbox_cfg),
                        port,
                    )));
                    self.registry.register(Arc::new(SandboxedTool::wrap(
                        crate::tools::read::ReadTool,
                        Arc::clone(&sandbox_cfg),
                        port,
                    )));
                    self.registry.register(Arc::new(SandboxedTool::wrap(
                        crate::tools::write::WriteTool,
                        Arc::clone(&sandbox_cfg),
                        port,
                    )));
                    Some(proxy)
                }
                Err(e) => {
                    warn!("Failed to start sandbox proxy: {e}");
                    None
                }
            }
        } else {
            None
        };

        let provider_name = crate::providers::provider_name_from_url(&self.config.base_url);
        let session = match SessionStore::open(&self.config.db_path).await {
            Ok(store) => {
                let result = match &self.resume_mode {
                    ResumeMode::New => {
                        store
                            .new_session(&self.agent_id, &self.config.model, &provider_name)
                            .await
                    }
                    ResumeMode::Resume { session_id } => store.load_session(session_id).await,
                };
                match result {
                    Ok(s) => {
                        info!(
                            agent_id = %self.agent_id,
                            session_id = %s.id,
                            model = %self.config.model,
                            provider = %provider_name,
                            "Session opened"
                        );
                        Some(s)
                    }
                    Err(e) => {
                        warn!("Failed to open session: {e}");
                        None
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to open session store at {:?}: {e}",
                    self.config.db_path
                );
                None
            }
        };

        Arc::new(KrabsAgent {
            agent_id: self.agent_id,
            config: self.config,
            provider: self.provider,
            registry: self.registry,
            memory: self.memory,
            permissions: self.permissions,
            system_prompt: self.system_prompt,
            skills: self.skills,
            hooks: self.hooks,
            session,
            _sandbox_proxy: sandbox_proxy,
            total_input_tokens: std::sync::atomic::AtomicU32::new(0),
            total_output_tokens: std::sync::atomic::AtomicU32::new(0),
        })
    }

    /// Sync build — no MCP, no session persistence.
    /// Prefer [`build_async`](Self::build_async) for production use.
    pub fn build(self) -> Arc<KrabsAgent> {
        Arc::new(KrabsAgent {
            agent_id: self.agent_id,
            config: self.config,
            provider: self.provider,
            registry: self.registry,
            memory: self.memory,
            permissions: self.permissions,
            system_prompt: self.system_prompt,
            skills: self.skills,
            hooks: self.hooks,
            session: None,
            _sandbox_proxy: None,
            total_input_tokens: std::sync::atomic::AtomicU32::new(0),
            total_output_tokens: std::sync::atomic::AtomicU32::new(0),
        })
    }
}

impl KrabsAgent {
    pub fn new(
        config: KrabsConfig,
        provider: impl LlmProvider + 'static,
        registry: ToolRegistry,
        memory: impl MemoryStore + 'static,
        permissions: PermissionGuard,
        system_prompt: String,
    ) -> Self {
        Self {
            agent_id: uuid::Uuid::new_v4().to_string(),
            config,
            provider: Box::new(provider),
            registry,
            memory: Box::new(memory),
            permissions,
            system_prompt,
            skills: None,
            hooks: HookRegistry::default(),
            session: None,
            _sandbox_proxy: None,
            total_input_tokens: std::sync::atomic::AtomicU32::new(0),
            total_output_tokens: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Sync skills from disk then return the full system prompt for this turn.
    ///
    /// The immutable base (SOUL + SYSTEM_PROMPT) is always prepended and cannot
    /// be overridden by any caller-supplied system prompt.
    async fn current_system_prompt(&self) -> String {
        let base = crate::prompts::base_system_prompt();

        let extension = match &self.skills {
            None => self.system_prompt.clone(),
            Some(registry) => {
                registry.sync().await;
                let section = registry.metadata_prompt().await;
                if section.is_empty() {
                    self.system_prompt.clone()
                } else {
                    format!("{}\n\n{}", self.system_prompt, section)
                }
            }
        };

        if extension.is_empty() {
            base
        } else {
            format!("{}\n\n{}", base, extension)
        }
    }

    /// Reconstruct the conversation history from the persisted session.
    ///
    /// - If a checkpoint exists, incomplete messages after the checkpoint boundary
    ///   are rolled back before loading.
    /// - If no checkpoint exists, all messages are loaded (best-effort).
    /// - Returns an empty `Vec` if no session is active.
    pub async fn load_history_from_session(&self) -> Result<Vec<Message>> {
        let session = match &self.session {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };

        let stored = match session.latest_checkpoint().await? {
            Some(cp) => {
                session.rollback_to(cp.last_msg_id).await?;
                session.messages_up_to(cp.last_msg_id).await?
            }
            None => session.messages().await?,
        };

        stored
            .iter()
            .map(crate::session::session::Session::stored_to_message)
            .collect()
    }

    /// Retry an async operation with exponential backoff, persisting each
    /// failure. Returns `Ok` on the first success, or `Err` after exhausting
    /// all attempts.
    async fn call_with_retry<F, Fut, T>(
        &self,
        turn: usize,
        context: &str,
        status_tx: Option<&mpsc::Sender<StreamChunk>>,
        mut f: F,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let max = self.config.max_retries;
        let base_ms = self.config.retry_base_delay_ms;

        for attempt in 0..=max {
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    self.persist_error(turn, context, &e, attempt).await;
                    if attempt < max {
                        let delay = base_ms * 2u64.pow(attempt as u32);
                        let msg = format!(
                            "↻ LLM attempt {}/{} failed: {e} — retrying in {delay}ms…",
                            attempt + 1,
                            max + 1,
                        );
                        warn!("{msg}");
                        if let Some(tx) = status_tx {
                            let _ = tx.send(StreamChunk::Status { text: msg }).await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!()
    }

    /// Call a tool with exponential-backoff retry on both hard errors (Err)
    /// and soft errors (ToolResult { is_error: true }).
    /// After exhausting retries, returns the final ToolResult for the LLM to handle.
    /// If `status_tx` is provided, a `StreamChunk::Status` is emitted on each retry.
    async fn call_tool_with_retry(
        &self,
        turn: usize,
        tool_name: &str,
        tool: Arc<dyn crate::tools::tool::Tool>,
        args: serde_json::Value,
        status_tx: Option<&mpsc::Sender<StreamChunk>>,
    ) -> crate::tools::tool::ToolResult {
        let max = self.config.tool_max_retries;
        let base_ms = self.config.retry_base_delay_ms;

        for attempt in 0..=max {
            match tool.call(args.clone()).await {
                Ok(result) if !result.is_error => return result,
                Ok(result) => {
                    if attempt < max {
                        let delay = base_ms * 2u64.pow(attempt as u32);
                        let msg = format!(
                            "↻ tool '{}' attempt {}/{} failed — retrying in {delay}ms…",
                            tool_name,
                            attempt + 1,
                            max + 1,
                        );
                        warn!("{msg}");
                        if let Some(tx) = status_tx {
                            let _ = tx.send(StreamChunk::Status { text: msg }).await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                    } else {
                        return result;
                    }
                }
                Err(e) => {
                    self.persist_error(turn, tool_name, &e, attempt).await;
                    if attempt < max {
                        let delay = base_ms * 2u64.pow(attempt as u32);
                        let msg = format!(
                            "↻ tool '{}' attempt {}/{} error: {e} — retrying in {delay}ms…",
                            tool_name,
                            attempt + 1,
                            max + 1,
                        );
                        warn!("{msg}");
                        if let Some(tx) = status_tx {
                            let _ = tx.send(StreamChunk::Status { text: msg }).await;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                    } else {
                        return crate::tools::tool::ToolResult::err(e.to_string());
                    }
                }
            }
        }
        unreachable!()
    }

    async fn write_checkpoint(&self, turn: usize) {
        if let Some(s) = &self.session {
            if let Err(e) = s.write_checkpoint(turn).await {
                warn!("Failed to write checkpoint: {e}");
            }
        }
    }

    /// Fire-and-log helper so persist errors never abort the agent loop.
    async fn persist_message(&self, msg: &Message, turn: usize) {
        if let Some(s) = &self.session {
            if let Err(e) = s.persist_message(msg, turn).await {
                warn!("Failed to persist message: {e}");
            }
        }
    }

    async fn persist_token_usage(&self, turn: usize, input: u32, output: u32) {
        if let Some(s) = &self.session {
            if let Err(e) = s.persist_token_usage(turn, input, output).await {
                warn!("Failed to persist token usage: {e}");
            }
        }
    }

    async fn persist_error(
        &self,
        turn: usize,
        context: &str,
        error: &anyhow::Error,
        attempt: usize,
    ) {
        if let Some(s) = &self.session {
            if let Err(e) = s.persist_error(turn, context, error, attempt).await {
                warn!("Failed to persist error: {e}");
            }
        }
    }

    pub fn total_tokens(&self) -> (u32, u32) {
        (
            self.total_input_tokens
                .load(std::sync::atomic::Ordering::Relaxed),
            self.total_output_tokens
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    pub fn context_used_pct(&self) -> f32 {
        let (inp, out) = self.total_tokens();
        let total = (inp + out) as f32;
        total / self.config.max_context_tokens as f32
    }

    pub async fn run_streaming(self: Arc<Self>, task: &str) -> Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(64);
        let task = task.to_string();
        let agent = Arc::clone(&self);

        tokio::task::spawn(async move {
            let system_prompt = agent.current_system_prompt().await;
            let messages = vec![Message::system(&system_prompt), Message::user(task.clone())];
            if let Err(e) = agent.streaming_loop_inner(task, messages, tx.clone()).await {
                let _ = tx
                    .send(StreamChunk::Done {
                        usage: crate::providers::provider::TokenUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                        },
                    })
                    .await;
                tracing::error!("streaming_loop failed: {e}");
            }
        });

        Ok(rx)
    }

    /// Run the streaming agent loop over an existing conversation history.
    ///
    /// `messages` should contain the full conversation so far, including the
    /// new user message at the end. The system prompt at position 0 (if any)
    /// is replaced each turn with the current computed system prompt.
    ///
    /// Returns a stream of `StreamChunk`s and a oneshot that fires with the
    /// final message list (including all new assistant + tool messages) when
    /// the agent loop completes, or `Err` if the loop fails.
    /// The session ID for this agent, if persistence is active.
    pub fn session_id(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.id.as_str())
    }

    pub async fn run_streaming_with_history(
        self: Arc<Self>,
        messages: Vec<Message>,
    ) -> Result<(
        mpsc::Receiver<StreamChunk>,
        oneshot::Receiver<Result<(Option<String>, Vec<Message>)>>,
    )> {
        let (tx, rx) = mpsc::channel(64);
        let (done_tx, done_rx) = oneshot::channel();
        let agent = Arc::clone(&self);
        let session_id = agent.session_id().map(|s| s.to_string());
        let task = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();

        tokio::task::spawn(async move {
            match agent.streaming_loop_inner(task, messages, tx.clone()).await {
                Ok(final_messages) => {
                    let _ = done_tx.send(Ok((session_id, final_messages)));
                }
                Err(e) => {
                    let _ = tx
                        .send(StreamChunk::Done {
                            usage: crate::providers::provider::TokenUsage {
                                input_tokens: 0,
                                output_tokens: 0,
                            },
                        })
                        .await;
                    let _ = done_tx.send(Err(e));
                }
            }
        });

        Ok((rx, done_rx))
    }

    /// Core streaming loop. `task` is used only for `AgentStart` hook event label.
    /// `messages` is the full initial conversation (system + history + user turn).
    async fn streaming_loop_inner(
        &self,
        task: String,
        mut messages: Vec<Message>,
        tx: mpsc::Sender<StreamChunk>,
    ) -> Result<Vec<Message>> {
        let tool_defs = self.registry.tool_defs();

        self.hooks
            .fire(&HookEvent::AgentStart { task: task.clone() })
            .await;

        // Persist the newest user message (the one just submitted, at the end of history).
        if let Some(user_msg) = messages.iter().rev().find(|m| matches!(m.role, Role::User)) {
            self.persist_message(user_msg, 0).await;
        }

        // Ensure a system message is at position 0 (only if non-empty)
        let system_prompt = self.current_system_prompt().await;
        if !system_prompt.is_empty() {
            if messages
                .first()
                .map(|m| matches!(m.role, Role::System))
                .unwrap_or(false)
            {
                messages[0] = Message::system(&system_prompt);
            } else {
                messages.insert(0, Message::system(&system_prompt));
            }
        } else if messages
            .first()
            .map(|m| matches!(m.role, Role::System))
            .unwrap_or(false)
        {
            messages.remove(0);
        }

        for turn in 0..self.config.max_turns {
            // If the consumer (CLI) dropped its receiver (e.g. Ctrl+C), stop immediately.
            if tx.is_closed() {
                return Ok(messages);
            }

            let system_prompt = self.current_system_prompt().await;
            if !system_prompt.is_empty() {
                if messages
                    .first()
                    .map(|m| matches!(m.role, Role::System))
                    .unwrap_or(false)
                {
                    messages[0] = Message::system(&system_prompt);
                } else {
                    messages.insert(0, Message::system(&system_prompt));
                }
            }

            self.hooks.fire(&HookEvent::TurnStart { turn }).await;

            if self.context_used_pct() > 0.8 {
                warn!(
                    "Context at {}%, trimming oldest messages",
                    (self.context_used_pct() * 100.0) as u32
                );
                self.trim_context(&mut messages);
            }

            debug!(
                "Stream turn {}: calling LLM with {} messages",
                turn,
                messages.len()
            );

            let (turn_tx, mut turn_rx) = mpsc::channel::<StreamChunk>(64);
            let turn_tx_retry = turn_tx.clone();
            self.call_with_retry(turn, "llm_stream", Some(&tx), || {
                let msgs = messages.clone();
                let defs = tool_defs.clone();
                let tx = turn_tx_retry.clone();
                async move { self.provider.stream_complete(&msgs, &defs, tx).await }
            })
            .await?;

            let mut delta_content = String::new();
            let mut tool_calls_this_turn = Vec::new();
            let mut usage_this_turn = None;

            while let Some(chunk) = turn_rx.recv().await {
                match &chunk {
                    StreamChunk::Delta { text } => delta_content.push_str(text),
                    StreamChunk::ToolCallReady { call } => {
                        tool_calls_this_turn.push(call.clone());
                    }
                    StreamChunk::Done { usage } => {
                        usage_this_turn = Some(usage.clone());
                    }
                    StreamChunk::Status { .. } => {}
                }
                if matches!(chunk, StreamChunk::Delta { .. } | StreamChunk::ToolCallReady { .. })
                    && tx.send(chunk).await.is_err()
                {
                    // Consumer dropped (Ctrl+C) — stop the loop.
                    return Ok(messages);
                }
            }

            if let Some(usage) = usage_this_turn {
                self.total_input_tokens
                    .fetch_add(usage.input_tokens, std::sync::atomic::Ordering::Relaxed);
                self.total_output_tokens
                    .fetch_add(usage.output_tokens, std::sync::atomic::Ordering::Relaxed);
                self.persist_token_usage(turn, usage.input_tokens, usage.output_tokens)
                    .await;
                let _ = tx.send(StreamChunk::Done { usage }).await;
            }

            if !tool_calls_this_turn.is_empty() {
                info!(
                    "Stream turn {}: got {} tool calls",
                    turn,
                    tool_calls_this_turn.len()
                );
                let assistant_msg = Message::assistant_tool_calls(tool_calls_this_turn.clone());
                self.persist_message(&assistant_msg, turn).await;
                messages.push(assistant_msg);

                for mut call in tool_calls_this_turn {
                    if !self.permissions.is_allowed(&call.name) {
                        let msg = format!("Permission denied for tool: {}", call.name);
                        warn!("{}", msg);
                        let result_msg = Message::tool_result(&msg, &call.id, &call.name);
                        self.persist_message(&result_msg, turn).await;
                        messages.push(result_msg);
                        continue;
                    }

                    // PreToolUse hook
                    let pre = self
                        .hooks
                        .fire(&HookEvent::PreToolUse {
                            tool_name: call.name.clone(),
                            args: call.args.clone(),
                            tool_use_id: call.id.clone(),
                        })
                        .await;

                    match pre {
                        HookOutput::ToolDecision(ToolUseDecision::Deny { reason }) => {
                            let msg = format!("Tool call denied by hook: {}", reason);
                            warn!("{}", msg);
                            let result_msg = Message::tool_result(&msg, &call.id, &call.name);
                            self.persist_message(&result_msg, turn).await;
                            messages.push(result_msg);
                            continue;
                        }
                        HookOutput::ToolDecision(ToolUseDecision::ModifyArgs { args }) => {
                            debug!("Hook modified args for tool: {}", call.name);
                            call.args = args;
                        }
                        _ => {}
                    }

                    match self.registry.get(&call.name) {
                        Some(tool) => {
                            debug!("Calling tool: {} with args: {}", call.name, call.args);
                            let result = self
                                .call_tool_with_retry(
                                    turn,
                                    &call.name,
                                    tool,
                                    call.args.clone(),
                                    Some(&tx),
                                )
                                .await;
                            let post = self
                                .hooks
                                .fire(&HookEvent::PostToolUse {
                                    tool_name: call.name.clone(),
                                    args: call.args.clone(),
                                    result: result.content.clone(),
                                    tool_use_id: call.id.clone(),
                                })
                                .await;
                            let content = if let HookOutput::AppendContext(ctx) = post {
                                format!("{}\n{}", result.content, ctx)
                            } else {
                                result.content
                            };
                            let result_msg = Message::tool_result(&content, &call.id, &call.name);
                            self.persist_message(&result_msg, turn).await;
                            messages.push(result_msg);
                        }
                        None => {
                            let msg = format!("Tool not found: {}", call.name);
                            warn!("{}", msg);
                            let result_msg = Message::tool_result(&msg, &call.id, &call.name);
                            self.persist_message(&result_msg, turn).await;
                            messages.push(result_msg);
                        }
                    }
                }

                self.write_checkpoint(turn).await;
                self.hooks.fire(&HookEvent::TurnEnd { turn }).await;
            } else {
                info!("Stream turn {}: final message received", turn);
                let final_msg = Message::assistant(&delta_content);
                self.persist_message(&final_msg, turn).await;
                messages.push(final_msg);
                self.write_checkpoint(turn).await;
                self.hooks.fire(&HookEvent::TurnEnd { turn }).await;
                self.hooks
                    .fire(&HookEvent::AgentStop {
                        result: delta_content,
                    })
                    .await;
                return Ok(messages);
            }
        }

        let e = anyhow::anyhow!("Max turns ({}) exceeded", self.config.max_turns);
        self.persist_error(self.config.max_turns, "max_turns", &e, 0)
            .await;
        Err(e)
    }

    fn trim_context(&self, messages: &mut Vec<Message>) {
        let system_count = messages
            .iter()
            .filter(|m| matches!(m.role, Role::System))
            .count();
        while messages.len() > system_count + 2 {
            let idx = messages
                .iter()
                .position(|m| !matches!(m.role, Role::System));
            if let Some(i) = idx {
                messages.remove(i);
            } else {
                break;
            }
        }
    }
}

#[async_trait]
impl Agent for KrabsAgent {
    async fn run(&self, task: &str) -> Result<AgentOutput> {
        let tool_defs = self.registry.tool_defs();

        self.hooks
            .fire(&HookEvent::AgentStart {
                task: task.to_string(),
            })
            .await;

        let system_prompt = self.current_system_prompt().await;
        let user_msg = Message::user(task);
        self.persist_message(&user_msg, 0).await;
        let mut messages = vec![Message::system(&system_prompt), user_msg];

        let mut tool_calls_made = 0;

        for turn in 0..self.config.max_turns {
            let system_prompt = self.current_system_prompt().await;
            messages[0] = Message::system(&system_prompt);

            self.hooks.fire(&HookEvent::TurnStart { turn }).await;

            if self.context_used_pct() > 0.8 {
                warn!(
                    "Context at {}%, trimming oldest messages",
                    (self.context_used_pct() * 100.0) as u32
                );
                self.trim_context(&mut messages);
            }

            debug!(
                "Turn {}: calling LLM with {} messages",
                turn,
                messages.len()
            );
            let response = self
                .call_with_retry(turn, "llm_complete", None, || {
                    let msgs = messages.clone();
                    let defs = tool_defs.clone();
                    async move { self.provider.complete(&msgs, &defs).await }
                })
                .await?;

            match response {
                LlmResponse::Message { content, usage } => {
                    info!(
                        "Turn {}: got final message ({} tokens)",
                        turn, usage.output_tokens
                    );
                    self.total_input_tokens
                        .fetch_add(usage.input_tokens, std::sync::atomic::Ordering::Relaxed);
                    self.total_output_tokens
                        .fetch_add(usage.output_tokens, std::sync::atomic::Ordering::Relaxed);
                    self.persist_token_usage(turn, usage.input_tokens, usage.output_tokens)
                        .await;
                    let final_msg = Message::assistant(&content);
                    self.persist_message(&final_msg, turn).await;
                    messages.push(final_msg);
                    self.write_checkpoint(turn).await;
                    self.hooks.fire(&HookEvent::TurnEnd { turn }).await;
                    self.hooks
                        .fire(&HookEvent::AgentStop {
                            result: content.clone(),
                        })
                        .await;
                    return Ok(AgentOutput {
                        result: content,
                        tool_calls_made,
                    });
                }
                LlmResponse::ToolCalls { calls, usage } => {
                    info!("Turn {}: got {} tool calls", turn, calls.len());
                    self.total_input_tokens
                        .fetch_add(usage.input_tokens, std::sync::atomic::Ordering::Relaxed);
                    self.total_output_tokens
                        .fetch_add(usage.output_tokens, std::sync::atomic::Ordering::Relaxed);
                    self.persist_token_usage(turn, usage.input_tokens, usage.output_tokens)
                        .await;

                    let assistant_msg = Message::assistant_tool_calls(calls.clone());
                    self.persist_message(&assistant_msg, turn).await;
                    messages.push(assistant_msg);

                    for mut call in calls {
                        tool_calls_made += 1;

                        if !self.permissions.is_allowed(&call.name) {
                            let msg = format!("Permission denied for tool: {}", call.name);
                            warn!("{}", msg);
                            let result_msg = Message::tool_result(&msg, &call.id, &call.name);
                            self.persist_message(&result_msg, turn).await;
                            messages.push(result_msg);
                            continue;
                        }

                        // PreToolUse hook
                        let pre = self
                            .hooks
                            .fire(&HookEvent::PreToolUse {
                                tool_name: call.name.clone(),
                                args: call.args.clone(),
                                tool_use_id: call.id.clone(),
                            })
                            .await;

                        match pre {
                            HookOutput::ToolDecision(ToolUseDecision::Deny { reason }) => {
                                let msg = format!("Tool call denied by hook: {}", reason);
                                warn!("{}", msg);
                                let result_msg = Message::tool_result(&msg, &call.id, &call.name);
                                self.persist_message(&result_msg, turn).await;
                                messages.push(result_msg);
                                continue;
                            }
                            HookOutput::ToolDecision(ToolUseDecision::ModifyArgs { args }) => {
                                debug!("Hook modified args for tool: {}", call.name);
                                call.args = args;
                            }
                            _ => {}
                        }

                        match self.registry.get(&call.name) {
                            Some(tool) => {
                                debug!("Calling tool: {} with args: {}", call.name, call.args);
                                let result = self
                                    .call_tool_with_retry(
                                        turn,
                                        &call.name,
                                        tool,
                                        call.args.clone(),
                                        None,
                                    )
                                    .await;
                                let post = self
                                    .hooks
                                    .fire(&HookEvent::PostToolUse {
                                        tool_name: call.name.clone(),
                                        args: call.args.clone(),
                                        result: result.content.clone(),
                                        tool_use_id: call.id.clone(),
                                    })
                                    .await;
                                let content = if let HookOutput::AppendContext(ctx) = post {
                                    format!("{}\n{}", result.content, ctx)
                                } else {
                                    result.content
                                };
                                let result_msg =
                                    Message::tool_result(&content, &call.id, &call.name);
                                self.persist_message(&result_msg, turn).await;
                                messages.push(result_msg);
                            }
                            None => {
                                let msg = format!("Tool not found: {}", call.name);
                                warn!("{}", msg);
                                let result_msg = Message::tool_result(&msg, &call.id, &call.name);
                                self.persist_message(&result_msg, turn).await;
                                messages.push(result_msg);
                            }
                        }
                    }
                }
            }

            self.write_checkpoint(turn).await;
            self.hooks.fire(&HookEvent::TurnEnd { turn }).await;
        }

        let e = anyhow::anyhow!("Max turns ({}) exceeded", self.config.max_turns);
        self.persist_error(self.config.max_turns, "max_turns", &e, 0)
            .await;
        Err(e)
    }
}
