use async_trait::async_trait;
use serde_json::json;

use crate::tools::tool::{McpContent, McpServerTool, McpToolResult};

pub struct EchoTool;

#[async_trait]
impl McpServerTool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Return the input arguments as JSON text. Useful for testing."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": true
        })
    }

    async fn call(&self, args: serde_json::Value) -> anyhow::Result<McpToolResult> {
        let text = serde_json::to_string_pretty(&args)?;
        Ok(McpToolResult {
            content: vec![McpContent::text(text)],
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_echo_returns_args_as_pretty_json() {
        let tool = EchoTool;
        let result = tool.call(json!({"hello": "world"})).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        let McpContent::Text { text } = &result.content[0];
        assert!(text.contains("hello"));
        assert!(text.contains("world"));
    }

    #[tokio::test]
    async fn test_echo_empty_args() {
        let tool = EchoTool;
        let result = tool.call(json!({})).await.unwrap();
        assert!(!result.is_error);
        assert!(!result.content.is_empty());
    }
}
