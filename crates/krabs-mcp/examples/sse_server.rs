use std::net::SocketAddr;

use krabs_mcp::McpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;

    let (_handle, server) = McpServer::new("krabs-mcp", "0.1.0")
        .with_builtins()
        .run_sse(addr)
        .await?;

    server.await
}
