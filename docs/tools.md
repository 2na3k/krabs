# Tools

Tools are the hands of a Krabs agent. An agent reasons with the LLM but *acts* through tools — reading files, running shell commands, fetching URLs, delegating to sub-agents. Every tool is a unit of capability that the agent can invoke by name.

---

## How tools work

When the LLM decides to call a tool it emits a structured `ToolCall` with a name and a JSON argument object. The agent loop looks up that name in the `ToolRegistry`, calls the tool, feeds the `ToolResult` back to the LLM, and continues.

```
LLM response
    └── ToolCall { name: "read", args: { "path": "src/main.rs" } }
              │
        ToolRegistry::get("read")
              │
         ReadTool::call(args)
              │
         ToolResult { content: "...", is_error: false }
              │
        appended to message history → next LLM turn
```

---

## Built-in tools

| Tool | Name sent to LLM | What it does |
|------|-----------------|--------------|
| `BashTool` | `bash` | Runs a shell command via `bash -c`, captures stdout + stderr |
| `ReadTool` | `read` | Reads a file, optionally with line offset and limit |
| `WriteTool` | `write` | Writes or patches a file |
| `GlobTool` | `glob` | Finds files matching a glob pattern |
| `GrepTool` | `grep` | Searches file contents with a regex |
| `WebFetchTool` | `web_fetch` | HTTP GET / POST, returns response body as text |
| `DelegateTool` | `delegate` | Spawns a child agent and returns its output |
| `DispatchTool` | `dispatch` | Dispatches work to multiple agents concurrently |
| `UserInputTool` | `user_input` | Pauses and asks the human for input |

All tools are registered in the `ToolRegistry`. The registry exposes them to the LLM via `tool_defs()` which serialises each tool's name, description, and JSON Schema parameters.

---

## The `Tool` trait

Every tool implements one trait:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;   // JSON Schema object
    async fn call(&self, args: serde_json::Value) -> Result<ToolResult>;
}
```

And `ToolResult` is:

```rust
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}
```

- `is_error: false` — success, content is the result
- `is_error: true` — the agent is told something went wrong; it can decide to retry, apologise, or abort

---

## Creating a new tool

### 1. Create the struct

Tools are plain unit structs (no state). If the tool genuinely needs initialised state — like a connection pool — hoist it to a `LazyLock` at module level so the struct itself stays stateless and can be constructed with just `ToolName`.

```rust
// crates/krabs-core/src/tools/my_tool.rs

use super::tool::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

pub struct MyTool;
```

### 2. Implement `Tool`

```rust
#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str {
        "my_tool"   // this is what the LLM uses to call it
    }

    fn description(&self) -> &str {
        "One sentence the LLM reads to decide whether to use this tool."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "The input value"
                }
            },
            "required": ["input"]
        })
    }

    async fn call(&self, args: serde_json::Value) -> Result<ToolResult> {
        let input = args["input"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'input' argument"))?;

        // do the work
        let output = format!("processed: {}", input);

        Ok(ToolResult::ok(output))
    }
}
```

Key rules:
- **Never `unwrap()`** — every failure path returns `Result` or `ToolResult::err(...)`.
- **Non-blocking** — use `tokio::fs`, `tokio::process`, `reqwest` etc. Never block the async runtime with synchronous I/O. Offload CPU-heavy work with `tokio::task::spawn_blocking`.
- **No unnecessary allocation** — borrow where a borrow will do.

### 3. Expose it from `tools/mod.rs`

```rust
// crates/krabs-core/src/tools/mod.rs
pub mod my_tool;
```

### 4. Re-export from `lib.rs` (if public API)

```rust
// crates/krabs-core/src/lib.rs
pub use tools::my_tool::MyTool;
```

### 5. Register it

**In the agent builder** — add it to the `ToolRegistry` before building:

```rust
let agent = KrabsAgentBuilder::new(config, provider)
    .registry({
        let mut r = ToolRegistry::new();
        r.register(Arc::new(BashTool));
        r.register(Arc::new(MyTool));   // ← here
        r
    })
    .build_async()
    .await;
```

**In the CLI** (`crates/krabs-cli/src/chat/commands.rs`):

```rust
pub(super) fn build_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(MyTool));   // ← here
    r
}
```

---

## Sandbox-aware tools

If your tool performs file I/O or network access it should respect the sandbox when enabled. You have two options:

### Option A — wrap with `SandboxedTool` (recommended)

`SandboxedTool<T>` is a generic wrapper that intercepts `call()` and applies path / domain checks before delegating to the inner tool. It handles `read`, `write`, `glob`, `grep`, and `bash` by tool name automatically. If your tool has a different name, calls pass through unchanged — you need Option B.

```rust
use krabs_core::sandbox::{SandboxConfig, SandboxedTool};

let sandboxed = SandboxedTool::wrap(MyTool, Arc::clone(&sandbox_cfg), proxy_port);
r.register(Arc::new(sandboxed));
```

The agent builder does this automatically for all built-in tools when `config.sandbox.enabled = true`.

### Option B — check inside `call()`

If your tool name isn't one of the built-in ones, add an explicit check:

```rust
async fn call(&self, args: serde_json::Value) -> Result<ToolResult> {
    let path = args["path"].as_str().ok_or_else(|| anyhow::anyhow!("missing path"))?;

    if let Some(ref cfg) = self.sandbox {
        if let Err(reason) = cfg.check_read_path(Path::new(path)) {
            return Ok(ToolResult::err(reason));
        }
    }

    // proceed
}
```

`SandboxConfig` exposes three check methods:

| Method | Use for |
|--------|---------|
| `check_read_path(&Path)` | Any tool that reads files |
| `check_write_path(&Path)` | Any tool that writes files |
| `check_domain(&str)` | Any tool that makes outbound network calls |

---

## Testing tools

Test each tool in isolation using `tokio::test`. Keep tests hermetic — mock any external dependency (HTTP servers, file paths) rather than hitting real networks or assuming filesystem layout.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn returns_error_on_missing_arg() {
        let result = MyTool.call(json!({})).await.unwrap();
        // or: assert!(MyTool.call(json!({})).await.is_err());
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn processes_input_correctly() {
        let result = MyTool
            .call(json!({ "input": "hello" }))
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(result.content, "processed: hello");
    }
}
```

For tools that make HTTP requests, spin up a local server in the test — see `tools/web_fetch.rs` for the pattern using `hyper` + `serve_once`.

**Never write tests that require internet access.** A test that fails without a network connection is a flaky test.
