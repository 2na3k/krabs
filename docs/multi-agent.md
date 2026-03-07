# Multi-Agent Architecture & Context Flow

## Overview

Krabs supports multi-agent orchestration through two complementary mechanisms:

1. **`DelegateTool`** — in-process sub-agents spawned on-demand by the LLM via tool call
2. **`MiniKrabsSpawner`** — parallel async tasks or isolated OS processes

---

## DelegateTool: Role-Specialized Sub-Agents

When the parent agent's LLM emits a `delegate` tool call, `DelegateTool` constructs a
brand-new `KrabsAgent` configured with a role-specific system prompt and runs the delegated
task to completion. The result is injected back into the parent's conversation as a
`tool_result` message, and the parent continues its loop.

```
┌─────────────────────────────────────────────────────────────┐
│                      KrabsAgent (parent)                    │
│                                                             │
│  system_prompt = base_system_prompt()                       │
│               + route_prefix (PLANNED / EXPLORE / REACTIVE) │
│               + role extension (e.g. planner.md)           │
│               + skills metadata section                     │
│                                                             │
│  memory: Box<dyn MemoryStore>  (own InMemory or SQLite)     │
│  session: Arc<Session>         (SQLite-persisted turns)     │
│  registry: ToolRegistry        (includes DelegateTool)      │
│  provider: Arc<dyn LlmProvider>  <- shared via Arc          │
│  permissions: PermissionGuard    <- cloned into sub-agents  │
│  config: KrabsConfig             <- cloned into sub-agents  │
└────────────────────┬────────────────────────────────────────┘
                     │  LLM emits tool_call { delegate, profile, task }
                     ▼
┌─────────────────────────────────────────────────────────────┐
│                       DelegateTool                          │
│                                                             │
│  call(args) {                                               │
│    profile = resolve_profile(args["profile"])               │
│    -> BaseAgent::Planner | FrontendDeveloper | Explorer     │
│                                                             │
│    agent = KrabsAgentBuilder::new(                          │
│      self.config.clone(),        // config passed by value  │
│      Arc::clone(&self.provider)  // provider shared         │
│    )                                                        │
│    .registry(self.registry.clone())  // tools cloned        │
│    .memory(InMemoryStore::new())     // fresh, isolated!    │
│    .permissions(self.permissions.clone())                   │
│    .system_prompt(profile.system_prompt()) // role-specific │
│    .build()                                                 │
│                                                             │
│    output = agent.run(task).await                           │
│    returns "[planner sub-agent — N calls]\n{result}"        │
│  }                                                          │
└─────────────────────────────────────────────────────────────┘
                     │  result injected back as tool_result message
                     ▼
         parent agent continues its loop
```

---

## Context Boundary: What Is Shared vs Isolated

| Thing | Parent -> Sub-agent | How |
|---|---|---|
| `config` (model, url, api key) | Passed | `.clone()` |
| `provider` (LLM client) | Shared | `Arc::clone` |
| `registry` (tools) | Shared structure | `.clone()` (Arc-wrapped tools inside) |
| `permissions` | Shared | `.clone()` |
| `memory` | **Fresh** | `InMemoryStore::new()` |
| `session` (conversation history) | **None** | sync `.build()`, no SQLite |
| `system_prompt` | **Overwritten** | role profile `.md` replaces parent's |
| `hooks` / telemetry | **Not forwarded** | not passed to builder |

Sub-agents are role-specialized but infrastructure-identical to the parent. They have the
same tools and provider, but start with a blank memory and no conversation history.

---

## System Prompt Layering

Every agent (parent or sub-agent) assembles its system prompt at call time:

```
current_system_prompt_for(decision)
  |
  +-- route_prefix              <- mode hint prepended first
  |   PLANNED mode:  decompose into subtasks, execute in order
  |   EXPLORE mode:  breadth-first, no fixed goal
  |   REACTIVE mode: no prefix (returned as-is)
  |
  +-- base_system_prompt()      <- immutable SOUL (prompts/system.rs)
  |                                cannot be overridden by any caller
  |
  +-- role extension            <- e.g. base_agent/planner.md
  |                                embedded at compile time via include_str!
  |
  +-- skills metadata           <- auto-appended if SkillRegistry has entries
```

The base system prompt is always the foundation. The role extension for sub-agents comes
from one of the built-in `BaseAgent` profiles:

| Profile | File | Purpose |
|---|---|---|
| `planner` | `base_agent/planner.md` | Decompose and plan complex tasks |
| `frontend_developer` | `base_agent/frontend_developer.md` | UI/frontend work |
| `explorer` | `base_agent/explorer.md` | Open-ended research and discovery |

---

## MiniKrabsSpawner: Parallel Execution

`MiniKrabsSpawner` runs multiple tasks concurrently against the same parent agent, or
spawns isolated OS processes for full separation.

```
MiniKrabsSpawner { agent: Arc<KrabsAgent> }
  |
  +-- spawn_task(task)
  |     tokio::spawn(Arc::clone(&agent).run(task))
  |     Arc shared — concurrent access, same memory/registry
  |
  +-- try_spawn_process(task)
  |     writes { task, config } to tmp JSON file
  |     exec: krabs-cli --task-json <tmp.json>
  |     only passes: model, base_url, api_key, max_turns
  |     full OS-level isolation; falls back to spawn_task on failure
  |
  +-- spawn_many(tasks, mode)
        spawns all tasks, then awaits all handles
        results returned in submission order
```

Process mode passes only the minimal config primitives across the boundary via a temporary
JSON file. Everything else (memory, registry, hooks, session) is fresh in the child process.

---

## Routing: How the Parent Decides What to Delegate

Before running its own loop, a `KrabsAgent` classifies the incoming task via `route()`:

```
route(task)
  |
  +-- config.router.mode == "planned"  -> RouteDecision::Planned
  +-- config.router.mode == "explore"  -> RouteDecision::Explore
  +-- config.router.mode == "reactive" -> RouteDecision::Reactive
  +-- config.router.mode == "auto"
        |
        +-- classifier == "llm"   -> single cheap LLM call, no tools
        +-- classifier == "rules" -> RulesRouter (keyword/heuristic match)
```

The decision affects only the system prompt prefix injected at the top. The agent loop
itself is the same in all modes.

---

## Data Flow Summary

```
User task
    |
    v
KrabsAgent.run(task)
    |
    +-- route(task) -> RouteDecision
    |
    +-- current_system_prompt_for(decision)
    |       assembles: route_prefix + base + role + skills
    |
    +-- load conversation history from Session (SQLite)
    |
    +-- [agent loop]
    |       provider.complete(messages, tools) -> LlmResponse
    |       |
    |       +-- Message  -> done, return AgentOutput
    |       |
    |       +-- ToolCalls -> for each call:
    |               |
    |               +-- permissions.check(tool_name)
    |               +-- registry.call(tool_name, args)
    |               |       if tool == "delegate":
    |               |           DelegateTool::call(args)
    |               |               -> build sub-agent
    |               |               -> sub-agent.run(task)
    |               |               -> return result string
    |               |
    |               +-- append tool_result to messages
    |               +-- persist to Session
    |               +-- loop back
    |
    +-- return AgentOutput { result, tool_calls_made }
```
