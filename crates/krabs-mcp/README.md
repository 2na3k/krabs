# krabs-mcp

Standalone MCP server crate for Krabs. Serves the [Model Context Protocol](https://spec.modelcontextprotocol.io) over **stdio** and **SSE** transports.

## Running

```bash
# build
cargo build --release -p krabs-mcp

# stdio (default) — used by Claude Desktop
./target/release/krabs-mcp

# SSE — used by web clients / remote callers
./target/release/krabs-mcp --sse              # 127.0.0.1:3000
./target/release/krabs-mcp --sse 0.0.0.0:8080
```

## Claude Desktop integration

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "krabs": {
      "command": "/absolute/path/to/target/release/krabs-mcp"
    }
  }
}
```

Restart Claude Desktop. The built-in tools will appear in the tool picker automatically.

## Library usage

```rust
use krabs_mcp::McpServer;

// stdio
McpServer::new("my-server", "0.1.0")
    .with_builtins()
    .run_stdio()
    .await?;

// SSE — returns a live handle for runtime tool registration + notifications
let (handle, server) = McpServer::new("my-server", "0.1.0")
    .with_builtins()
    .run_sse("127.0.0.1:3000".parse()?)
    .await?;

tokio::spawn(server);

// Register a tool at runtime — all connected clients notified automatically
handle.register_tool(Arc::new(MyTool)).await;
```

## Built-in tools

| Tool | Description |
|---|---|
| `echo` | Returns input args as pretty JSON. Useful for testing. |
| `web_fetch` | GET/POST any URL via `reqwest`. |
| `web_search` | DuckDuckGo instant answers — no API key needed. |

Call `.with_builtins()` to register all three, or `.register(Arc::new(MyTool))` for your own.

## Structure

```
src/
├── protocol/   # JSON-RPC 2.0 types + MCP wire types
├── server/     # handler, stdio transport, SSE transport, McpServerHandle
└── tools/      # McpServerTool trait, registry, built-ins
```
