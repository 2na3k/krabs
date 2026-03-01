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

# Planning loops
```text
1. Take a look at CLAUDE.md for the core rules, strictly following them.
2. Search for everything related on the web.
3. Planning, and always breaking them into a composable roadmap for development.
4. Save the plan into the `docs/`, don't just let you only know about what you're doing.
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

# Core rules
- Every byte allocated is a byte that must earn its keep. No unnecessary heap allocations. No lazy clones when a borrow will do. Resource efficiency isn't a nice-to-have — it's the *law*.
- An agent that waits is an agent that fails. Krabs moves. Tasks are dispatched, tracked, and completed with urgency. Idle cycles are wasted money, and Krabs *hates* wasted money.
- The borrow checker is not the enemy — it's the first mate. Krabs agents are correct by construction. If it compiles, it ships. Memory safety isn't a constraint; it's a competitive advantage.
- Krabs builds pipelines from clear, composable parts: tools, agents, memory, and planners snapping together like crab claws. Simple interfaces, powerful combinations.
- Every action an agent takes is logged, traceable, and inspectable. Krabs keeps the books. You will always know what your agents did, why they did it, and how much it cost.
- Each layer is a trait. Swap any layer without touching the others. Krabs is opinionated about structure, not about your specific choices within it.
- Krabs is built on `tokio`. Agents run concurrently. Tool calls are non-blocking.
- There is no `unwrap()` in production Krabs code. Every failure path is a `Result`. Every agent step that can fail, says so in its type signature. You always know what can go wrong.
