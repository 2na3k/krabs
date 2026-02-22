# `CLAUDE.md`: Instruction for agents

Krabs (or Mr. Krabs) is an agentic framework doing a lot of things.

# Project structure
```text
crates/
├── krabs-core        # core logic
├── krabs-cli         # CLI entry
├── krabs-server      # backend (binary: goosed)
└── krabs-mcp         # MCP extensions
```

# Development loops
```text
1. Make changes
2. cargo fmt
3. cargo build
4. cargo test -p <crate>
5. cargo clippy --all-targets -- -D warnings
```

# Entrypoint
- `krabs-core`: `./crates/krabs-core/agents/agents.rs`
- `krabs-cli`: `./crates/krabs-cli/krabs-cli/main.rs`
