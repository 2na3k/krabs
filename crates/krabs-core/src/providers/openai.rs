use super::provider::{LlmProvider, LlmResponse, Message, Role, StreamChunk, TokenUsage, ToolCall};
use crate::tools::tool::ToolDef;
use anyhow::Result;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;

pub struct OpenAiProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiProvider {
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

fn build_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let mut obj = json!({ "role": role, "content": m.content });
            if let Some(id) = &m.tool_call_id {
                obj["tool_call_id"] = json!(id);
            }
            obj
        })
        .collect()
}

fn build_tools(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.parameters } })
        })
        .collect()
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, messages: &[Message], tools: &[ToolDef]) -> Result<LlmResponse> {
        let msgs = build_messages(messages);
        let tools_val = build_tools(tools);

        let mut body = json!({ "model": self.model, "messages": msgs });
        if !tools_val.is_empty() {
            body["tools"] = json!(tools_val);
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let data: Value = resp.json().await?;

        let usage = {
            let u = &data["usage"];
            TokenUsage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
            }
        };

        let choice = &data["choices"][0];
        let message = &choice["message"];
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("stop");

        if finish_reason == "tool_calls" || message["tool_calls"].is_array() {
            let tool_calls = message["tool_calls"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|tc| {
                            let id = tc["id"].as_str()?.to_string();
                            let name = tc["function"]["name"].as_str()?.to_string();
                            let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                            let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                            Some(ToolCall { id, name, args })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(LlmResponse::ToolCalls {
                calls: tool_calls,
                usage,
            })
        } else {
            let content = message["content"].as_str().unwrap_or("").to_string();
            Ok(LlmResponse::Message { content, usage })
        }
    }

    async fn stream_complete(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        tx: mpsc::Sender<StreamChunk>,
    ) -> Result<()> {
        let msgs = build_messages(messages);
        let tools_val = build_tools(tools);

        let mut body = json!({
            "model": self.model,
            "messages": msgs,
            "stream": true,
            "stream_options": { "include_usage": true }
        });
        if !tools_val.is_empty() {
            body["tools"] = json!(tools_val);
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        // Accumulate tool call argument deltas: index -> (id, name, args)
        let mut tool_calls: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();
        // Track the latest usage seen (some providers send it on every chunk, e.g. Gemini)
        let mut last_usage: Option<TokenUsage> = None;
        let mut byte_stream = resp.bytes_stream();
        let mut leftover = String::new();

        'outer: while let Some(chunk) = byte_stream.next().await {
            let bytes = chunk?;
            leftover.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete lines
            while let Some(pos) = leftover.find('\n') {
                let line = leftover[..pos].trim_end_matches('\r').to_string();
                leftover = leftover[pos + 1..].to_string();

                if !line.starts_with("data: ") {
                    continue;
                }
                let data = &line["data: ".len()..];
                if data == "[DONE]" {
                    break 'outer;
                }

                let delta: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Track usage whenever present (works for both OpenAI and Gemini)
                if let Some(usage) = delta.get("usage").filter(|u| !u.is_null()) {
                    last_usage = Some(TokenUsage {
                        input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                        output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
                    });
                }

                // Skip chunks with no choices (usage-only terminal chunks from OpenAI)
                let choices = delta["choices"].as_array();
                if choices.map(|c| c.is_empty()).unwrap_or(true) {
                    continue;
                }

                let choice = &delta["choices"][0];
                let msg_delta = &choice["delta"];
                let finish_reason = choice["finish_reason"].as_str().unwrap_or("");

                // Content delta
                if let Some(text) = msg_delta["content"].as_str() {
                    if !text.is_empty() {
                        let _ = tx.send(StreamChunk::Delta { text: text.to_string() }).await;
                    }
                }

                // Tool call deltas
                if let Some(tc_arr) = msg_delta["tool_calls"].as_array() {
                    for tc in tc_arr {
                        let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                        let entry = tool_calls.entry(idx).or_insert_with(|| {
                            (String::new(), String::new(), String::new())
                        });
                        if let Some(id) = tc["id"].as_str() {
                            entry.0 = id.to_string();
                        }
                        if let Some(name) = tc["function"]["name"].as_str() {
                            entry.1 = name.to_string();
                        }
                        if let Some(args) = tc["function"]["arguments"].as_str() {
                            entry.2.push_str(args);
                        }
                    }
                }

                // Flush tool calls when complete
                if finish_reason == "tool_calls" {
                    let mut indices: Vec<usize> = tool_calls.keys().cloned().collect();
                    indices.sort();
                    for idx in indices {
                        if let Some((id, name, args_str)) = tool_calls.remove(&idx) {
                            let args: Value =
                                serde_json::from_str(&args_str).unwrap_or(json!({}));
                            let _ = tx
                                .send(StreamChunk::ToolCallReady {
                                    call: ToolCall { id, name, args },
                                })
                                .await;
                        }
                    }
                }
            }
        }

        // Flush any tool calls not yet flushed (e.g. Gemini uses finish_reason "stop"
        // instead of "tool_calls" for function call responses)
        if !tool_calls.is_empty() {
            let mut indices: Vec<usize> = tool_calls.keys().cloned().collect();
            indices.sort();
            for idx in indices {
                if let Some((id, name, args_str)) = tool_calls.remove(&idx) {
                    let args: Value = serde_json::from_str(&args_str).unwrap_or(json!({}));
                    let _ = tx
                        .send(StreamChunk::ToolCallReady {
                            call: ToolCall { id, name, args },
                        })
                        .await;
                }
            }
        }

        // Send exactly one Done with the final usage
        let usage = last_usage.unwrap_or(TokenUsage { input_tokens: 0, output_tokens: 0 });
        let _ = tx.send(StreamChunk::Done { usage }).await;

        Ok(())
    }
}
