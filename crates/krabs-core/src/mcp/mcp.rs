use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

use crate::tools::tool::Tool;

use super::client::McpClient;
use super::tool::{McpReadResourceTool, McpTool};

// ── Server config ────────────────────────────────────────────────────────────

/// Persisted MCP server entry in `~/.krabs/mcp.json`.
/// Supports both stdio (subprocess) and SSE (HTTP) transports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    /// Transport type: "stdio" or "sse" (defaults to "sse" if command is empty)
    #[serde(default)]
    pub transport: String,
    /// For stdio: the executable to spawn
    #[serde(default)]
    pub command: String,
    /// For stdio: arguments to pass to the executable
    #[serde(default)]
    pub args: Vec<String>,
    /// For SSE: the base URL of the MCP server
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl McpServer {
    pub fn stdio(name: impl Into<String>, command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            name: name.into(),
            transport: "stdio".into(),
            command: command.into(),
            args,
            url: String::new(),
            enabled: true,
        }
    }

    pub fn sse(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: "sse".into(),
            command: String::new(),
            args: vec![],
            url: url.into(),
            enabled: true,
        }
    }

    pub fn transport_label(&self) -> &str {
        if !self.transport.is_empty() {
            &self.transport
        } else if !self.command.is_empty() {
            "stdio"
        } else {
            "sse"
        }
    }

    pub fn endpoint(&self) -> String {
        if !self.url.is_empty() {
            self.url.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }

    async fn connect(&self) -> Result<McpClient> {
        let label = self.transport_label();
        if label == "stdio" {
            McpClient::connect_stdio(&self.name, &self.command, &self.args).await
        } else {
            McpClient::connect_sse(&self.name, &self.url).await
        }
    }
}

// ── Registry (persisted) ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpRegistry {
    pub servers: Vec<McpServer>,
}

impl McpRegistry {
    fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".krabs")
            .join("mcp.json")
    }

    pub async fn load() -> Self {
        let path = Self::path();
        tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub async fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(&path, json).await?;
        Ok(())
    }

    pub fn add(&mut self, server: McpServer) {
        self.servers.retain(|s| s.name != server.name);
        self.servers.push(server);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.servers.len();
        self.servers.retain(|s| s.name != name);
        self.servers.len() < before
    }

    /// Connect all enabled servers and return a `LiveMcpRegistry` with active connections.
    pub async fn connect_all(self) -> LiveMcpRegistry {
        let mut clients = Vec::new();
        for server in &self.servers {
            if !server.enabled {
                continue;
            }
            match server.connect().await {
                Ok(client) => {
                    info!("MCP server '{}' connected", server.name);
                    clients.push(Arc::new(client));
                }
                Err(e) => {
                    warn!("MCP server '{}' failed to connect: {}", server.name, e);
                }
            }
        }
        LiveMcpRegistry { clients }
    }
}

// ── LiveMcpRegistry — holds active connections ───────────────────────────────

pub struct LiveMcpRegistry {
    pub clients: Vec<Arc<McpClient>>,
}

impl LiveMcpRegistry {
    /// Discover tools from all connected servers, returning `Box<dyn Tool>`
    /// ready to be registered in a `ToolRegistry`.
    pub async fn tools_for_all(&self) -> Vec<Box<dyn Tool>> {
        let mut tools: Vec<Box<dyn Tool>> = Vec::new();
        for client in &self.clients {
            match client.list_tools().await {
                Ok(infos) => {
                    for info in infos {
                        tools.push(Box::new(McpTool::new(
                            Arc::clone(client),
                            info.name,
                            info.description,
                            info.input_schema,
                        )));
                    }
                    tools.push(Box::new(McpReadResourceTool::new(Arc::clone(client))));
                }
                Err(e) => {
                    warn!(
                        "MCP server '{}' tools/list failed: {}",
                        client.server_name, e
                    );
                }
            }
        }
        tools
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    pub fn server_names(&self) -> Vec<&str> {
        self.clients
            .iter()
            .map(|c| c.server_name.as_str())
            .collect()
    }
}
