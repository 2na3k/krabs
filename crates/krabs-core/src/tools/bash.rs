use super::tool::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Execute a bash shell command and return stdout/stderr output."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The bash command to execute" },
                "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default: 30)", "default": 30 }
            },
            "required": ["command"]
        })
    }
    async fn call(&self, args: serde_json::Value) -> Result<ToolResult> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' argument"))?;
        let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(30);
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            Command::new("bash").arg("-c").arg(command).output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Command timed out after {}s", timeout_secs))?
        .map_err(|e| anyhow::anyhow!("Failed to execute command: {}", e))?;
        let mut content = String::new();
        if !output.stdout.is_empty() {
            content.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("stderr: ");
            content.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        let is_error = !output.status.success();
        if content.is_empty() {
            content = if is_error {
                format!("Command failed with exit code {:?}", output.status.code())
            } else {
                "(no output)".to_string()
            };
        }
        Ok(ToolResult { content, is_error })
    }
}
