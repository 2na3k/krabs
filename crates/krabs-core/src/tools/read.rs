use super::tool::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read the contents of a file."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read" },
                "offset": { "type": "integer", "description": "Line number to start reading from (1-indexed)" },
                "limit": { "type": "integer", "description": "Maximum number of lines to read" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, args: serde_json::Value) -> Result<ToolResult> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing path argument"))?;
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::err(format!("Failed to read {}: {}", path, e))),
        };
        let offset = args["offset"].as_u64().unwrap_or(1).saturating_sub(1) as usize;
        let limit = args["limit"].as_u64().map(|l| l as usize);
        let lines: Vec<&str> = content.lines().collect();
        let slice = &lines[offset.min(lines.len())..];
        let slice = if let Some(l) = limit {
            &slice[..l.min(slice.len())]
        } else {
            slice
        };
        Ok(ToolResult::ok(slice.join("\n")))
    }
}
