# `krabs-cli`

Interactive TUI for Krabs. Streaming output, slash commands, multi-agent support, and persona switching — all in the terminal.

## Running

```bash
cargo run -p krabs-cli
```

First run triggers a setup wizard to configure your API provider and credentials.

## Interface

The CLI uses `ratatui` for rendering and `crossterm` for raw terminal input. The main loop:

- Displays a scrollable message history
- Streams agent output token-by-token with a spinner during tool execution
- Shows tool calls inline (tool name, args, result)
- Tracks token usage per session

## Slash commands

| Command           | Description                              |
|-------------------|------------------------------------------|
| `/tools`          | List all tools registered to the agent   |
| `/skills`         | List loaded skills and their descriptions|
| `/models <name>`  | Switch to a different model mid-session  |
| `/agents list`    | List available agent personas            |
| `/mcp list`       | List configured MCP servers              |
| `/hooks list`     | List active hooks                        |
| `/usage`          | Show input/output token counts           |

## Personas

Activate a persona with `@<name>` syntax. Personas are loaded from `./krabs/agents/*.md` files.

**Example persona file (`krabs/agents/reviewer.md`):**
```markdown
---
name: reviewer
description: Code review specialist
model: claude-sonnet-4-6
---

You are a senior Rust engineer focused on correctness and performance...
```

Switching personas changes the system prompt and optionally the model for that session.

## Multi-agent

Parallel agents can be spawned from the CLI and their outputs are labeled and interleaved in the display.

## Keyboard shortcuts

| Key        | Action                        |
|------------|-------------------------------|
| `Enter`    | Submit message                |
| `Ctrl+C`   | Quit                          |
| `↑ / ↓`   | Scroll message history        |
| `Ctrl+L`   | Clear screen                  |

## Setup

On first run (or via `krabs setup`), you'll be prompted for:

- Provider (`anthropic`, `openai`, or `gemini`)
- API key
- Default model


## Architecture

```
main.rs         entry point; loads credentials, dispatches to setup or chat
setup.rs        credential configuration wizard
chat.rs         main TUI loop — rendering, input, streaming, slash commands
user_input.rs   raw mode keyboard handling via crossterm
```
