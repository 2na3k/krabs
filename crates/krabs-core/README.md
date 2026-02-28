# `krabs-core`

The agentic engine powering Krabs. Everything that makes an agent think, act, remember, and stop.

## Modules

### `agents`

Core agent runtime. The `KrabsAgent` struct runs a streaming agentic loop: call the LLM, execute tools, loop until a final message or max turns.

```rust
let agent = KrabsAgentBuilder::new()
    .config(config)
    .provider(provider)
    .tool(BashTool::new())
    .build()?;

let output = agent.run("summarize the logs in /var/log").await?;
```

The `Agent` trait is the public interface:

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    async fn run(&self, task: &str) -> Result<AgentOutput>;
}
```

### `providers`

LLM provider abstraction. Swap between Anthropic, OpenAI, and Gemini without changing any agent code.

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, messages: &[Message], tools: &[ToolDef]) -> Result<LlmResponse>;
    async fn stream_complete(&self, messages: &[Message], tools: &[ToolDef], tx: mpsc::Sender<StreamChunk>) -> Result<()>;
}
```

Providers: `AnthropicProvider`, `OpenAiProvider`, `GeminiProvider`.

### `tools`

Every tool implements:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;
    async fn call(&self, args: serde_json::Value) -> Result<ToolResult>;
}
```

Built-in tools:

| Tool      | Description                              |
|-----------|------------------------------------------|
| `bash`    | Execute shell commands with timeout      |
| `read`    | Read a file from the filesystem          |
| `write`   | Write content to a file                  |
| `glob`    | Find files matching a pattern            |
| `grep`    | Search file contents with regex          |

Tools are registered in a `ToolRegistry` and resolved by name at runtime.

### `hooks`

Intercept agent lifecycle events. Hooks run before/after tool use, at turn boundaries, and at agent start/stop.

```rust
#[async_trait]
pub trait Hook: Send + Sync {
    fn matcher(&self) -> Option<&str>;  // regex over tool names
    async fn on_event(&self, event: &HookEvent) -> Result<HookOutput>;
}
```

Events: `AgentStart`, `TurnStart`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `TurnEnd`, `AgentStop`.

`PreToolUse` resolution order (highest wins):
1. `Deny` — block the tool call
2. `ModifyArgs` — replace arguments before execution
3. `Continue` — proceed normally

Other events can return `Stop`, `SystemMessage`, `AppendContext`, or `Continue`.

### `skills`

Filesystem-based skills using progressive disclosure. Metadata is always in the system prompt; full instructions load on demand via `read_skill()`.

**Directory layout:**
```
skills/
└── my-skill/
    ├── SKILL.md          # frontmatter + instructions
    └── reference.md      # optional bundled resources
```

**SKILL.md format:**
```markdown
---
name: my-skill
description: Does X and Y
---

Instructions here.
```

The `SkillRegistry` hot-reloads at the start of every agent turn.

### `mcp`

Model Context Protocol integration. Connect external tool servers via stdio or SSE transport. MCP tools are wrapped into the `Tool` trait and namespaced as `mcp__{server}__{tool}`.

**Config (`~/.krabs/mcp.json`):**
```json
{
  "servers": [
    {
      "name": "filesystem",
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
      "enabled": true
    }
  ]
}
```

### `config`

`KrabsConfig` and `Credentials`. Resolution order:

1. Environment variables (`KRABS_MODEL`, `ANTHROPIC_API_KEY`, etc.)
2. `~/.krabs/config.json`
3. `.krabs.json` in the working directory

See [`docs/config-schema.md`](../../docs/config-schema.md) for the full schema.

### `memory`

Simple key-value store for agent memory.

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn set(&self, key: &str, value: &str) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Option<String>>;
    async fn delete(&self, key: &str) -> Result<()>;
    async fn keys(&self) -> Result<Vec<String>>;
}
```

Default: `InMemoryStore`. Swap in any backend that implements the trait.

### `permissions`

Allow/deny lists for tool access. Checked before `PreToolUse` hooks.

```rust
let guard = PermissionGuard::new()
    .allow(["read", "glob"])   // only these tools
    .deny(["bash"]);           // or block specific tools
```

## Agent loop

```
AgentStart
└── for each turn:
    ├── sync skills
    ├── trim context if >80% used
    ├── TurnStart
    ├── LLM stream_complete()
    │   └── for each tool call:
    │       ├── PreToolUse  →  Deny / ModifyArgs / Allow
    │       ├── permission check
    │       ├── tool.call()
    │       └── PostToolUse / PostToolUseFailure
    └── TurnEnd
└── AgentStop  (final message or max_turns exceeded)
```

## Token tracking

`KrabsAgent` tracks `total_input_tokens` and `total_output_tokens` via `AtomicU32`. Context usage is checked at the start of each turn and oldest messages are trimmed when approaching the limit.
