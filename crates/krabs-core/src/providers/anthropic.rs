use super::provider::{LlmProvider, LlmResponse, Message, Role, StreamChunk, TokenUsage, ToolCall};
use crate::tools::tool::ToolDef;
use anyhow::Result;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;

pub struct AnthropicProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }
}

fn build_anthropic_messages(messages: &[Message]) -> (Option<String>, Vec<Value>) {
    let mut system_parts = Vec::new();
    let mut msgs = Vec::new();

    for m in messages {
        match m.role {
            Role::System => system_parts.push(m.content.clone()),
            Role::User => msgs.push(json!({ "role": "user", "content": m.content })),
            Role::Assistant => msgs.push(json!({ "role": "assistant", "content": m.content })),
            Role::Tool => {
                // Anthropic tool results go as user messages with tool_result content blocks
                let id = m.tool_call_id.clone().unwrap_or_default();
                msgs.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": m.content
                    }]
                }));
            }
        }
    }

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n"))
    };

    (system, msgs)
}

fn build_anthropic_tools(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters
            })
        })
        .collect()
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, messages: &[Message], tools: &[ToolDef]) -> Result<LlmResponse> {
        let (tx, mut rx) = mpsc::channel(256);
        self.stream_complete(messages, tools, tx).await?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = TokenUsage { input_tokens: 0, output_tokens: 0 };

        while let Some(chunk) = rx.recv().await {
            match chunk {
                StreamChunk::Delta { text } => content.push_str(&text),
                StreamChunk::ToolCallReady { call } => tool_calls.push(call),
                StreamChunk::Done { usage: u } => usage = u,
            }
        }

        if !tool_calls.is_empty() {
            Ok(LlmResponse::ToolCalls { calls: tool_calls, usage })
        } else {
            Ok(LlmResponse::Message { content, usage })
        }
    }

    async fn stream_complete(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        tx: mpsc::Sender<StreamChunk>,
    ) -> Result<()> {
        let (system, msgs) = build_anthropic_messages(messages);
        let tools_val = build_anthropic_tools(tools);

        let mut body = json!({
            "model": self.model,
            "max_tokens": 8096,
            "messages": msgs,
            "stream": true
        });

        if let Some(sys) = system {
            body["system"] = json!(sys);
        }
        if !tools_val.is_empty() {
            body["tools"] = json!(tools_val);
        }

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        // Accumulate tool use blocks: index -> (id, name, args_json)
        let mut tool_blocks: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();
        let mut current_block_idx: Option<usize> = None;
        let mut byte_stream = resp.bytes_stream();
        let mut leftover = String::new();

        while let Some(chunk) = byte_stream.next().await {
            let bytes = chunk?;
            leftover.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(pos) = leftover.find('\n') {
                let line = leftover[..pos].trim_end_matches('\r').to_string();
                leftover = leftover[pos + 1..].to_string();

                if line.starts_with("event: ") {
                    // track event type via next data line — handled below
                    continue;
                }

                if !line.starts_with("data: ") {
                    continue;
                }

                let data = &line["data: ".len()..];
                let ev: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let ev_type = ev["type"].as_str().unwrap_or("");

                match ev_type {
                    "content_block_start" => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        let block = &ev["content_block"];
                        if block["type"].as_str() == Some("tool_use") {
                            let id = block["id"].as_str().unwrap_or("").to_string();
                            let name = block["name"].as_str().unwrap_or("").to_string();
                            tool_blocks.insert(idx, (id, name, String::new()));
                            current_block_idx = Some(idx);
                        } else {
                            current_block_idx = None;
                        }
                    }
                    "content_block_delta" => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        let delta = &ev["delta"];
                        let delta_type = delta["type"].as_str().unwrap_or("");

                        if delta_type == "text_delta" {
                            if let Some(text) = delta["text"].as_str() {
                                if !text.is_empty() {
                                    let _ = tx.send(StreamChunk::Delta { text: text.to_string() }).await;
                                }
                            }
                        } else if delta_type == "input_json_delta" {
                            if let Some(partial) = delta["partial_json"].as_str() {
                                if let Some(entry) = tool_blocks.get_mut(&idx) {
                                    entry.2.push_str(partial);
                                }
                            }
                        }
                        let _ = current_block_idx; // suppress warning
                    }
                    "content_block_stop" => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        if let Some((id, name, args_str)) = tool_blocks.remove(&idx) {
                            let args: Value =
                                serde_json::from_str(&args_str).unwrap_or(json!({}));
                            let _ = tx
                                .send(StreamChunk::ToolCallReady {
                                    call: ToolCall { id, name, args, thought_signature: None },
                                })
                                .await;
                        }
                    }
                    "message_delta" => {
                        if let Some(usage) = ev.get("usage") {
                            let tok = TokenUsage {
                                input_tokens: ev["usage"]["input_tokens"]
                                    .as_u64()
                                    .unwrap_or(0) as u32,
                                output_tokens: usage["output_tokens"].as_u64().unwrap_or(0) as u32,
                            };
                            let _ = tx.send(StreamChunk::Done { usage: tok }).await;
                        }
                    }
                    "message_start" => {
                        // initial usage (input tokens)
                        // we'll get final usage in message_delta
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }
}
