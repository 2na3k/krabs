pub mod handler;
pub mod sse;
pub mod stdio;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use tokio::sync::{Mutex, RwLock};

use crate::protocol::jsonrpc::JsonRpcNotification;
use crate::server::sse::{NotificationBroadcaster, SessionMap};
use crate::tools::builtin::{echo::EchoTool, web_fetch::WebFetchTool, web_search::WebSearchTool};
use crate::tools::registry::McpToolRegistry;
use crate::tools::tool::McpServerTool;

pub struct McpServer {
    name: String,
    version: String,
    registry: McpToolRegistry,
}

/// Live handle returned by `McpServer::run_sse`.
///
/// Use it to push notifications to all connected clients or to register new
/// tools at runtime without restarting the server.
pub struct McpServerHandle {
    registry: Arc<RwLock<McpToolRegistry>>,
    broadcaster: NotificationBroadcaster,
}

impl McpServerHandle {
    /// Register a new tool at runtime and immediately notify all connected
    /// clients with `notifications/tools/list_changed`.
    pub async fn register_tool(&self, tool: Arc<dyn McpServerTool>) {
        self.registry.write().await.register(tool);
        self.broadcaster
            .broadcast(&JsonRpcNotification::new(
                "notifications/tools/list_changed",
            ))
            .await;
    }

    /// Access the broadcaster directly to send arbitrary notifications.
    pub fn broadcaster(&self) -> &NotificationBroadcaster {
        &self.broadcaster
    }
}

impl McpServer {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            registry: McpToolRegistry::new(),
        }
    }

    /// Register all built-in tools (`echo`, `web_fetch`, `web_search`).
    pub fn with_builtins(self) -> Self {
        self.register(Arc::new(EchoTool))
            .register(Arc::new(WebFetchTool))
            .register(Arc::new(WebSearchTool))
    }

    /// Register a tool at startup (builder-style, before the server starts).
    pub fn register(mut self, tool: Arc<dyn McpServerTool>) -> Self {
        self.registry.register(tool);
        self
    }

    /// Run on stdio. Blocks until EOF.
    pub async fn run_stdio(self) -> anyhow::Result<()> {
        let registry = Arc::new(RwLock::new(self.registry));
        stdio::run_stdio(registry, &self.name, &self.version).await
    }

    /// Bind the SSE server and return a live handle plus the accept-loop future.
    ///
    /// ```no_run
    /// # use std::net::SocketAddr;
    /// # use std::sync::Arc;
    /// # use krabs_mcp::{McpServer, tools::builtin::echo::EchoTool};
    /// # #[tokio::main] async fn main() -> anyhow::Result<()> {
    /// let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    /// let (handle, server) = McpServer::new("demo", "0.1")
    ///     .register(Arc::new(EchoTool))
    ///     .run_sse(addr)
    ///     .await?;
    ///
    /// tokio::spawn(server);
    ///
    /// // Later — add a tool and all connected clients are notified:
    /// handle.register_tool(Arc::new(EchoTool)).await;
    /// # Ok(()) }
    /// ```
    pub async fn run_sse(
        self,
        addr: SocketAddr,
    ) -> anyhow::Result<(McpServerHandle, BoxFuture<'static, anyhow::Result<()>>)> {
        let sessions: SessionMap = Arc::new(Mutex::new(HashMap::new()));
        let registry = Arc::new(RwLock::new(self.registry));
        let broadcaster = NotificationBroadcaster::new(sessions.clone());
        let handle = McpServerHandle {
            registry: registry.clone(),
            broadcaster,
        };
        let fut = Box::pin(sse::run_sse_loop(
            registry,
            self.name,
            self.version,
            addr,
            sessions,
        ));
        Ok((handle, fut))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::echo::EchoTool;

    #[tokio::test]
    async fn test_register_tool_broadcasts_list_changed() {
        let sessions: SessionMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        sessions.lock().await.insert(uuid::Uuid::new_v4(), tx);

        let broadcaster = NotificationBroadcaster::new(sessions);
        let registry = Arc::new(RwLock::new(McpToolRegistry::new()));
        let handle = McpServerHandle {
            registry,
            broadcaster,
        };

        handle.register_tool(Arc::new(EchoTool)).await;

        let msg = rx.recv().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["method"], "notifications/tools/list_changed");
    }

    #[tokio::test]
    async fn test_register_tool_adds_to_registry() {
        let sessions: SessionMap = Arc::new(Mutex::new(HashMap::new()));
        let broadcaster = NotificationBroadcaster::new(sessions);
        let registry = Arc::new(RwLock::new(McpToolRegistry::new()));
        let handle = McpServerHandle {
            registry: registry.clone(),
            broadcaster,
        };

        assert!(registry.read().await.get("echo").is_none());
        handle.register_tool(Arc::new(EchoTool)).await;
        assert!(registry.read().await.get("echo").is_some());
    }
}
