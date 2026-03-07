use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;
use tracing::warn;

use crate::protocol::jsonrpc::JsonRpcRequest;
use crate::server::handler::dispatch;
use crate::tools::registry::McpToolRegistry;

pub async fn run_stdio(
    registry: Arc<RwLock<McpToolRegistry>>,
    server_name: &str,
    server_version: &str,
) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to parse JSON-RPC request: {e}");
                continue;
            }
        };

        if let Some(response) = dispatch(&registry, server_name, server_version, req).await {
            let mut serialized = serde_json::to_string(&response)?;
            serialized.push('\n');
            stdout.write_all(serialized.as_bytes()).await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}
