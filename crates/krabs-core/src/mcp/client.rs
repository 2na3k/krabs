use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::info;

use super::transport::{SseTransport, StdioTransport, Transport};

const PROTOCOL_VERSION: &str = "2024-11-05";

// ── MCP capability types ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "mimeType", default)]
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType", default)]
    pub mime_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub blob: Option<String>, // base64 encoded
}

// ── McpClient ────────────────────────────────────────────────────────────────

pub struct McpClient {
    pub server_name: String,
    transport: Transport,
}

impl McpClient {
    /// Connect to a stdio MCP server by spawning a subprocess.
    pub async fn connect_stdio(
        server_name: impl Into<String>,
        command: &str,
        args: &[String],
    ) -> Result<Self> {
        let transport = Transport::Stdio(Box::new(StdioTransport::spawn(command, args)?));
        let mut client = Self {
            server_name: server_name.into(),
            transport,
        };
        client.initialize().await?;
        Ok(client)
    }

    /// Connect to an HTTP/SSE MCP server.
    pub async fn connect_sse(server_name: impl Into<String>, url: &str) -> Result<Self> {
        let transport = Transport::Sse(SseTransport::new(url));
        let mut client = Self {
            server_name: server_name.into(),
            transport,
        };
        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "roots": { "listChanged": false },
                "sampling": {}
            },
            "clientInfo": {
                "name": "krabs",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let result = self.transport.request("initialize", Some(params)).await?;
        let server_name = result["serverInfo"]["name"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        info!(
            "MCP connected: {} (protocol {})",
            server_name,
            result["protocolVersion"].as_str().unwrap_or("?")
        );

        // Send initialized notification
        self.transport
            .notify("notifications/initialized", None)
            .await?;
        Ok(())
    }

    /// Discover all tools exposed by this server.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>> {
        let result = self.transport.request("tools/list", None).await?;
        let tools = result["tools"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("tools/list: expected 'tools' array"))?;

        tools
            .iter()
            .map(|t| serde_json::from_value(t.clone()).map_err(Into::into))
            .collect()
    }

    /// Call a tool and return its result content as a string.
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<(String, bool)> {
        let params = json!({
            "name": tool_name,
            "arguments": arguments
        });
        let result = self.transport.request("tools/call", Some(params)).await?;

        let is_error = result["isError"].as_bool().unwrap_or(false);
        let content = result["content"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|c| {
                if c["type"] == "text" {
                    c["text"].as_str().map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok((content, is_error))
    }

    /// Discover resources exposed by this server.
    pub async fn list_resources(&self) -> Result<Vec<McpResourceInfo>> {
        let result = self.transport.request("resources/list", None).await?;
        let resources = match result["resources"].as_array() {
            Some(r) => r,
            None => return Ok(vec![]),
        };
        resources
            .iter()
            .map(|r| serde_json::from_value(r.clone()).map_err(Into::into))
            .collect()
    }

    /// Read a resource by URI.
    pub async fn read_resource(&self, uri: &str) -> Result<Vec<McpResourceContent>> {
        let params = json!({ "uri": uri });
        let result = self
            .transport
            .request("resources/read", Some(params))
            .await?;
        let contents = match result["contents"].as_array() {
            Some(c) => c,
            None => bail!("resources/read: expected 'contents' array"),
        };
        contents
            .iter()
            .map(|c| serde_json::from_value(c.clone()).map_err(Into::into))
            .collect()
    }
}
