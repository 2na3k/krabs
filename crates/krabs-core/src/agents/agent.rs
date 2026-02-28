use crate::config::KrabsConfig;
use crate::memory::MemoryStore;
use crate::permissions::PermissionGuard;
use crate::providers::provider::{LlmProvider, LlmResponse, Message, Role, StreamChunk};
use crate::skills::registry::SkillRegistry;
use crate::tools::read_skill::ReadSkillTool;
use crate::tools::registry::ToolRegistry;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

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
    pub config: KrabsConfig,
    pub provider: Box<dyn LlmProvider>,
    pub registry: ToolRegistry,
    pub memory: Box<dyn MemoryStore>,
    pub permissions: PermissionGuard,
    pub system_prompt: String,
    pub skills: Option<Arc<SkillRegistry>>,
    total_input_tokens: std::sync::atomic::AtomicU32,
    total_output_tokens: std::sync::atomic::AtomicU32,
}

pub struct KrabsAgentBuilder {
    config: KrabsConfig,
    provider: Box<dyn LlmProvider>,
    registry: ToolRegistry,
    memory: Box<dyn MemoryStore>,
    permissions: PermissionGuard,
    system_prompt: String,
    skills: Option<Arc<SkillRegistry>>,
}

impl KrabsAgentBuilder {
    pub fn new(config: KrabsConfig, provider: impl LlmProvider + 'static) -> Self {
        Self {
            config,
            provider: Box::new(provider),
            registry: ToolRegistry::default(),
            memory: Box::new(crate::memory::memory::InMemoryStore::new()),
            permissions: PermissionGuard::new(),
            system_prompt: String::new(),
            skills: None,
        }
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

    pub fn build(self) -> Arc<KrabsAgent> {
        Arc::new(KrabsAgent {
            config: self.config,
            provider: self.provider,
            registry: self.registry,
            memory: self.memory,
            permissions: self.permissions,
            system_prompt: self.system_prompt,
            skills: self.skills,
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
            config,
            provider: Box::new(provider),
            registry,
            memory: Box::new(memory),
            permissions,
            system_prompt,
            skills: None,
            total_input_tokens: std::sync::atomic::AtomicU32::new(0),
            total_output_tokens: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Sync skills from disk then return the full system prompt for this turn.
    async fn current_system_prompt(&self) -> String {
        match &self.skills {
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
            if let Err(e) = agent.streaming_loop(task, tx.clone()).await {
                // Surface the error as a Done chunk so the caller isn't left hanging
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

    async fn streaming_loop(&self, task: String, tx: mpsc::Sender<StreamChunk>) -> Result<()> {
        let tool_defs = self.registry.tool_defs();
        let system_prompt = self.current_system_prompt().await;
        let mut messages = vec![Message::system(&system_prompt), Message::user(task)];

        for turn in 0..self.config.max_turns {
            // Sync skills and rebuild system message each turn to pick up
            // any skills the user dropped in while the agent is running.
            let system_prompt = self.current_system_prompt().await;
            messages[0] = Message::system(&system_prompt);

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
            self.provider
                .stream_complete(&messages, &tool_defs, turn_tx)
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
                }
                if matches!(
                    chunk,
                    StreamChunk::Delta { .. } | StreamChunk::ToolCallReady { .. }
                ) {
                    let _ = tx.send(chunk).await;
                }
            }

            if let Some(usage) = usage_this_turn {
                self.total_input_tokens
                    .fetch_add(usage.input_tokens, std::sync::atomic::Ordering::Relaxed);
                self.total_output_tokens
                    .fetch_add(usage.output_tokens, std::sync::atomic::Ordering::Relaxed);
                let _ = tx.send(StreamChunk::Done { usage }).await;
            }

            if !tool_calls_this_turn.is_empty() {
                info!(
                    "Stream turn {}: got {} tool calls",
                    turn,
                    tool_calls_this_turn.len()
                );
                let calls_summary = tool_calls_this_turn
                    .iter()
                    .map(|c| format!("[tool_call: {}({})]", c.name, c.args))
                    .collect::<Vec<_>>()
                    .join(", ");
                messages.push(Message::assistant(calls_summary));

                for call in tool_calls_this_turn {
                    if !self.permissions.is_allowed(&call.name) {
                        let msg = format!("Permission denied for tool: {}", call.name);
                        warn!("{}", msg);
                        messages.push(Message::tool_result(&msg, &call.id));
                        continue;
                    }
                    match self.registry.get(&call.name) {
                        Some(tool) => {
                            debug!("Calling tool: {} with args: {}", call.name, call.args);
                            let result = tool.call(call.args).await?;
                            messages.push(Message::tool_result(&result.content, &call.id));
                        }
                        None => {
                            let msg = format!("Tool not found: {}", call.name);
                            warn!("{}", msg);
                            messages.push(Message::tool_result(&msg, &call.id));
                        }
                    }
                }
            } else {
                info!("Stream turn {}: final message received", turn);
                messages.push(Message::assistant(&delta_content));
                return Ok(());
            }
        }

        anyhow::bail!("Max turns ({}) exceeded", self.config.max_turns)
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

        let system_prompt = self.current_system_prompt().await;
        let mut messages = vec![Message::system(&system_prompt), Message::user(task)];

        let mut tool_calls_made = 0;

        for turn in 0..self.config.max_turns {
            // Sync skills and rebuild system message each turn to pick up
            // any skills the user dropped in while the agent is running.
            let system_prompt = self.current_system_prompt().await;
            messages[0] = Message::system(&system_prompt);

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
            let response = self.provider.complete(&messages, &tool_defs).await?;

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
                    messages.push(Message::assistant(&content));
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

                    let calls_summary = calls
                        .iter()
                        .map(|c| format!("[tool_call: {}({})]", c.name, c.args))
                        .collect::<Vec<_>>()
                        .join(", ");
                    messages.push(Message::assistant(calls_summary));

                    for call in calls {
                        tool_calls_made += 1;

                        if !self.permissions.is_allowed(&call.name) {
                            let msg = format!("Permission denied for tool: {}", call.name);
                            warn!("{}", msg);
                            messages.push(Message::tool_result(&msg, &call.id));
                            continue;
                        }

                        match self.registry.get(&call.name) {
                            Some(tool) => {
                                debug!("Calling tool: {} with args: {}", call.name, call.args);
                                let result = tool.call(call.args).await?;
                                messages.push(Message::tool_result(&result.content, &call.id));
                            }
                            None => {
                                let msg = format!("Tool not found: {}", call.name);
                                warn!("{}", msg);
                                messages.push(Message::tool_result(&msg, &call.id));
                            }
                        }
                    }
                }
            }
        }

        anyhow::bail!("Max turns ({}) exceeded", self.config.max_turns)
    }
}
