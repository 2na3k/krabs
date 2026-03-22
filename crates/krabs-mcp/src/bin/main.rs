use std::net::SocketAddr;

use krabs_mcp::McpServer;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// krabs-mcp — standalone MCP server
///
/// Usage:
///   krabs-mcp                        run on stdio (default, for Claude Desktop)
///   krabs-mcp --sse                  run SSE server on 127.0.0.1:3000
///   krabs-mcp --sse 0.0.0.0:8080    run SSE server on a custom address
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let mut args = std::env::args().skip(1);

    match args.next().as_deref() {
        None => {
            // Default: stdio transport — what Claude Desktop expects.
            McpServer::new("krabs-mcp", VERSION)
                .with_builtins()
                .run_stdio()
                .await
        }

        Some("--sse") => {
            let addr: SocketAddr = args
                .next()
                .unwrap_or_else(|| "127.0.0.1:3000".to_string())
                .parse()?;

            let (_handle, server) = McpServer::new("krabs-mcp", VERSION)
                .with_builtins()
                .run_sse(addr)
                .await?;

            server.await
        }

        Some("--version" | "-V") => {
            println!("krabs-mcp {VERSION}");
            Ok(())
        }

        Some(arg) => {
            eprintln!("Unknown argument: {arg}");
            eprintln!("Usage: krabs-mcp [--sse [addr:port]]");
            std::process::exit(1);
        }
    }
}
