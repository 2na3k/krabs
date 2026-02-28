# Krabs

An agentic framework built in Rust — fast, composable, and fully observable.

> They got openclawd. I got Mr. Krabs.

## What it is

Krabs is a multi-provider agentic framework designed around a single principle: agents should be correct by construction, traceable by default, and composable by design.

Built on `tokio` for async-first execution. Every layer is a trait. Every failure is a `Result`. No `unwrap()` in production.

## Project layout

```
crates/
├── krabs-core     # agent engine, providers, tools, hooks, skills, MCP
├── krabs-cli      # interactive TUI — the main way to use Krabs
├── krabs-server   # HTTP backend (in progress)
└── krabs-mcp      # MCP server extensions (in progress)
```

## Quick start

```bash
cargo build --release
./target/release/krabs
```

On first run, you'll be prompted to configure your API credentials. You can also set environment variables:

```
ANTHROPIC_API_KEY=...
OPENAI_API_KEY=...
GEMINI_API_KEY=...
```

## Providers

Krabs supports three LLM providers out of the box:

| Provider  | Models                          |
|-----------|---------------------------------|
| Anthropic | claude-opus-4.6, claude-sonnet-4.6, etc. |
| OpenAI    | gpt-5.1, gpt-5.2-mini, etc.       |
| Gemini    | gemini-3.0-flash, etc.          |

Switch models mid-session with `/models <model-name>`.

## Core features

**Tools** — Built-in: `bash`, `read`, `write`, `glob`, `grep`. Add your own by implementing the `Tool` trait.

**Skills** — Drop a `SKILL.md` in your `skills/` directory. The agent loads metadata at startup and fetches full instructions on demand. Skills hot-reload every turn.

**Hooks** — React to agent lifecycle events: `AgentStart`, `PreToolUse`, `PostToolUse`, `AgentStop`, and more. Block, modify, or augment tool calls without touching agent logic.

**MCP** — Connect any MCP-compatible server via stdio or SSE transport. Tools appear namespaced as `mcp__{server}__{tool}`.

**Personas** — Place `*.md` files in `krabs/agents/`. Invoke with `@<name>` in the chat.

**Permissions** — Per-agent allow/deny lists for tool access.

## CLI commands

Inside the chat TUI:

| Command           | Description                          |
|-------------------|--------------------------------------|
| `/tools`          | List available tools                 |
| `/skills`         | List loaded skills                   |
| `/models <name>`  | Switch model                         |
| `/agents list`    | List agents                          |
| `/mcp list`       | List MCP servers                     |
| `/hooks list`     | List active hooks                    |
| `/usage`          | Token usage for current session      |
| `@<name>`         | Activate a persona                   |

## Configuration

Krabs resolves config from multiple sources in order:

1. Environment variables
2. `~/.krabs/config.json` (global)
3. `.krabs.json` (project-level override)

See [`docs/config-schema.md`](docs/config-schema.md) for the full schema.

## Architecture principles

- Every byte allocated earns its keep. No lazy clones when a borrow will do.
- Each layer is a trait. Swap the LLM provider, memory store, or tool registry without touching the rest.
- Every agent action is logged, traceable, and token-accounted.
- Agents run concurrently on `tokio`. Tool calls are non-blocking.
- No `unwrap()`. Every failure path is a `Result`.

## Crate docs

- [`krabs-core`](crates/krabs-core/README.md) — the agent engine
- [`krabs-cli`](crates/krabs-cli/README.md) — the interactive interface
