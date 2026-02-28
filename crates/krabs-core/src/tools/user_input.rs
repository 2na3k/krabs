use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::tools::tool::{Tool, ToolResult};

// ── request types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    ChooseOne,
    ChooseMany,
}

/// A user-input request the agent sends to the TUI.
pub struct UserInputRequest {
    pub mode: InputMode,
    pub question: String,
    /// Up to 4 options. A "custom…" option is always appended by the TUI.
    pub options: Vec<String>,
    /// Resolved with the user's answer text when they confirm.
    pub respond: oneshot::Sender<String>,
}

// ── tool ──────────────────────────────────────────────────────────────────────

/// A tool that pauses the agent and asks the user a structured question.
///
/// Two modes:
/// - `choose_one`  — radio selection: pick exactly one option or type a custom answer.
/// - `choose_many` — checkbox selection: pick any subset and/or add a custom note.
///
/// The agent blocks until the user confirms. The answer is returned as plain text.
pub struct UserInputTool {
    tx: mpsc::Sender<UserInputRequest>,
}

impl UserInputTool {
    pub fn new(tx: mpsc::Sender<UserInputRequest>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Tool for UserInputTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Pause and ask the user a structured question before continuing. \
         Use `choose_one` when exactly one answer is needed (e.g. which database, \
         which approach). Use `choose_many` when multiple selections are valid \
         (e.g. which features to enable). \
         Provide 2–4 short options; a free-text custom option is always added automatically. \
         Only call this when user input is genuinely required to proceed."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["choose_one", "choose_many"],
                    "description": "choose_one: user picks a single option. choose_many: user picks any subset."
                },
                "question": {
                    "type": "string",
                    "description": "The question to display to the user. Be concise and specific."
                },
                "options": {
                    "type": "array",
                    "minItems": 2,
                    "maxItems": 4,
                    "items": { "type": "string" },
                    "description": "2–4 short options for the user to choose from. A free-text custom option is always appended."
                }
            },
            "required": ["mode", "question", "options"]
        })
    }

    async fn call(&self, args: Value) -> Result<ToolResult> {
        let mode = match args["mode"].as_str().unwrap_or("choose_one") {
            "choose_many" => InputMode::ChooseMany,
            _ => InputMode::ChooseOne,
        };

        let question = args["question"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required field: question"))?
            .to_string();

        let options: Vec<String> = args["options"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing required field: options"))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .take(4)
            .collect();

        if options.len() < 2 {
            anyhow::bail!("ask_user requires at least 2 options");
        }

        let (respond, rx) = oneshot::channel::<String>();

        self.tx
            .send(UserInputRequest {
                mode,
                question,
                options,
                respond,
            })
            .await
            .map_err(|_| anyhow::anyhow!("TUI channel closed — cannot ask user"))?;

        let answer = rx
            .await
            .map_err(|_| anyhow::anyhow!("user closed the input prompt"))?;

        Ok(ToolResult {
            content: answer,
            is_error: false,
        })
    }
}
