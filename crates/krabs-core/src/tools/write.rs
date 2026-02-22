use super::tool::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to write the file" },
                "content": { "type": "string", "description": "Content to write" },
                "old_string": { "type": "string", "description": "For patch mode: string to replace" },
                "new_string": { "type": "string", "description": "For patch mode: replacement string" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, args: serde_json::Value) -> Result<ToolResult> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;
        if let (Some(old), Some(new)) = (args["old_string"].as_str(), args["new_string"].as_str()) {
            let existing = match tokio::fs::read_to_string(path).await {
                Ok(c) => c,
                Err(e) => return Ok(ToolResult::err(format!("Failed to read {}: {}", path, e))),
            };
            if !existing.contains(old) {
                return Ok(ToolResult::err(format!("old_string not found in {}", path)));
            }
            let updated = existing.replacen(old, new, 1);
            tokio::fs::write(path, updated)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            return Ok(ToolResult::ok(format!("Patched {}", path)));
        }
        let content = args["content"].as_str().unwrap_or("");
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, content)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!(
            "Written {} bytes to {}",
            content.len(),
            path
        )))
    }
}
