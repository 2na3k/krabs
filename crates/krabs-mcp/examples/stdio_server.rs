use krabs_mcp::McpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    McpServer::new("krabs-mcp", "0.1.0")
        .with_builtins()
        .run_stdio()
        .await
}
