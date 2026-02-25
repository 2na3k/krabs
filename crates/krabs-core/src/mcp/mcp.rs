use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub url: String,
    pub enabled: bool,
}

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
}

// Legacy stub kept for compatibility
pub struct McpClient {
    _server_url: String,
}

impl McpClient {
    pub fn new(server_url: impl Into<String>) -> Self {
        Self {
            _server_url: server_url.into(),
        }
    }
}
