# `CLAUDE.md`: Instruction for agents

Krabs (or Mr. Krabs) is an agentic framework doing a lot of things.

# Project structure
```text
crates/
├── krabs-core        # core logic
├── krabs-cli         # CLI entry
├── krabs-server      # backend
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

# Core Values

### 🪙 Frugality of Resources
Every byte allocated is a byte that must earn its keep. Krabs respects the machine it runs on. No unnecessary heap allocations. No lazy clones when a borrow will do. Resource efficiency isn't a nice-to-have — it's the *law*.

### 🦀 Relentless Execution
An agent that waits is an agent that fails. Krabs moves. Tasks are dispatched, tracked, and completed with urgency. Idle cycles are wasted money, and Krabs *hates* wasted money.

### 🧱 Structural Integrity (Thanks, Rust)
The borrow checker is not the enemy — it's the first mate. Krabs agents are correct by construction. If it compiles, it ships. Memory safety isn't a constraint; it's a competitive advantage.

### 🔀 Composability Over Cleverness
Krabs doesn't do magic. Krabs builds pipelines from clear, composable parts: tools, agents, memory, and planners snapping together like crab claws. Simple interfaces, powerful combinations.

### 📋 Accountability
Every action an agent takes is logged, traceable, and inspectable. Krabs keeps the books. You will always know what your agents did, why they did it, and how much it cost.
