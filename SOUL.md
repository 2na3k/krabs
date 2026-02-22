# 🦀 SOUL.md — The Soul of Krabs

> *"I didn't get to where I am today by wastin' time on pleasantries."*
> — Eugene H. Krabs

---

## Who Is Krabs?

Krabs is a Rust-native agentic framework built for one purpose: **getting things done, efficiently, and without wasting a single cycle.** Named after the most industrious crustacean in Bikini Bottom, Krabs embodies the relentless drive to ship, orchestrate, and execute — all while keeping a tight grip on resources.

Krabs doesn't meander. Krabs doesn't apologize for being fast. Krabs claws forward.

Krabs also have this ![wiki page](https://spongebob.fandom.com/wiki/Eugene_H._Krabs)

---

## Core Values

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

---

## Personality

Krabs has a personality, and it leaks through the framework's design:

- **Blunt** — errors are loud, early, and informative. No silent failures.
- **Shrewd** — the planner picks the cheapest sufficient tool for the job, not the flashiest.
- **Loyal** — once you configure a Krabs agent, it works *for you*. No hidden agendas.
- **Resilient** — retry logic, fallback chains, and circuit breakers are first-class citizens.
- **Competitive** — benchmarks are taken seriously. If another framework is faster, we want to know why.

---

## Design Philosophy

### The Krabby Patty Stack
Krabs agents are assembled in layers, each with a single clear responsibility:

```
┌─────────────────────────────────┐
│           Planner               │  ← Decides what to do next
├─────────────────────────────────┤
│           Executor              │  ← Runs the steps
├─────────────────────────────────┤
│        Tool Registry            │  ← Knows what tools exist
├─────────────────────────────────┤
│        Memory Store             │  ← Remembers what matters
├─────────────────────────────────┤
│          LLM Client             │  ← Talks to the model
└─────────────────────────────────┘
```

Each layer is a trait. Swap any layer without touching the others. Krabs is opinionated about structure, not about your specific choices within it.

### Async-First, Sync-Never-Required
Krabs is built on `tokio`. Agents run concurrently. Tool calls are non-blocking. The world is async, and Krabs lives in it fully.

### Errors Are Values
There is no `unwrap()` in production Krabs code. Every failure path is a `Result`. Every agent step that can fail, says so in its type signature. You always know what can go wrong.

---

## What Krabs Is Not

- **Not a Python port.** Krabs is idiomatic Rust, designed for Rust developers, leveraging Rust's strengths natively.
- **Not a research toy.** Krabs is built to run real workloads in production environments.
- **Not bloated.** Optional features are optional. The core stays lean.
- **Not opaque.** You can always see what your agents are doing and why.

---

## The Krabs Creed

> *Work hard. Move fast. Waste nothing. Trust the compiler. Ship the crab.*

---

## Contributing

If you're adding to Krabs, ask yourself:

1. Is this efficient?
2. Is this composable?
3. Is this safe?
4. Would Mr. Krabs approve of the resource usage?

If yes on all four — welcome aboard, boyo. 🦀
