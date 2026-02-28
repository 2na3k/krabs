use crate::tools::tool::ToolDef;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_call_id: Option<String>,
    /// Tool name — populated on Tool role messages (required by some providers)
    pub tool_name: Option<String>,
    /// Populated on assistant messages that requested tool calls
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: None,
        }
    }
    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: String::new(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Some(calls),
        }
    }
    pub fn tool_result(
        content: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            tool_calls: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
    /// Gemini thinking models attach a thought_signature that must be echoed back
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug)]
pub enum LlmResponse {
    Message {
        content: String,
        usage: TokenUsage,
    },
    ToolCalls {
        calls: Vec<ToolCall>,
        usage: TokenUsage,
    },
}

#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// Incremental text token from the model
    Delta { text: String },
    /// Tool call ready (args fully accumulated)
    ToolCallReady { call: ToolCall },
    /// Final usage stats, signals end of stream
    Done { usage: TokenUsage },
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, messages: &[Message], tools: &[ToolDef]) -> Result<LlmResponse>;

    async fn stream_complete(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        tx: mpsc::Sender<StreamChunk>,
    ) -> Result<()>;
}

/// Allow `Arc<dyn LlmProvider>` to be used wherever `impl LlmProvider` is expected.
#[async_trait]
impl LlmProvider for std::sync::Arc<dyn LlmProvider> {
    async fn complete(&self, messages: &[Message], tools: &[ToolDef]) -> Result<LlmResponse> {
        (**self).complete(messages, tools).await
    }

    async fn stream_complete(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        tx: mpsc::Sender<StreamChunk>,
    ) -> Result<()> {
        (**self).stream_complete(messages, tools, tx).await
    }
}
