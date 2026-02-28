use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::tools::tool::{Tool, ToolResult};

use super::client::McpClient;

/// Wraps an MCP server tool as a Krabs `Tool`.
///
/// Registered in the tool registry as `mcp__{server}__{tool}`.
pub struct McpTool {
    pub client: Arc<McpClient>,
    pub tool_name: String,
    pub description: String,
    pub schema: Value,
    pub registered_name: String,
}

impl McpTool {
    pub fn new(
        client: Arc<McpClient>,
        tool_name: impl Into<String>,
        description: impl Into<String>,
        schema: Value,
    ) -> Self {
        let tool_name = tool_name.into();
        let registered_name = format!("mcp__{}__{}", client.server_name, tool_name);
        Self {
            client,
            tool_name,
            description: description.into(),
            schema,
            registered_name,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.registered_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.schema.clone()
    }

    async fn call(&self, args: Value) -> Result<ToolResult> {
        match self.client.call_tool(&self.tool_name, args).await {
            Ok((content, is_error)) => Ok(ToolResult { content, is_error }),
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}

/// Exposes MCP resources as a readable tool.
///
/// Registered as `mcp__{server}__read_resource`.
pub struct McpReadResourceTool {
    pub client: Arc<McpClient>,
    pub registered_name: String,
}

impl McpReadResourceTool {
    pub fn new(client: Arc<McpClient>) -> Self {
        let registered_name = format!("mcp__{}__read_resource", client.server_name);
        Self {
            client,
            registered_name,
        }
    }
}

#[async_trait]
impl Tool for McpReadResourceTool {
    fn name(&self) -> &str {
        &self.registered_name
    }

    fn description(&self) -> &str {
        "Read a resource from the MCP server by URI"
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "The resource URI to read"
                }
            },
            "required": ["uri"]
        })
    }

    async fn call(&self, args: Value) -> Result<ToolResult> {
        let uri = args["uri"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("uri is required"))?;

        match self.client.read_resource(uri).await {
            Ok(contents) => {
                let text = contents
                    .iter()
                    .filter_map(|c| c.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(ToolResult::ok(text))
            }
            Err(e) => Ok(ToolResult::err(e.to_string())),
        }
    }
}
