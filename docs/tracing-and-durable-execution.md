# Tracing & Durable Execution in Krabs

## 1. Tracing / Logging

### Stack
- Crate: `tracing` v0.1
- No explicit subscriber/filter initialization in the codebase — consumers set that up
- No `#[instrument]` spans; logging is flat and strategic

### Log Levels in Use

| Level   | Where / What                                                                 |
|---------|------------------------------------------------------------------------------|
| `info!` | Session open (agent_id, session_id, model, provider), final message tokens  |
| `debug!`| Turn info, tool invocation args, hook-modified args                          |
| `warn!` | Retry attempts, failed checkpoints, failed message/token/error persistence, context trimming, permission/tool issues |
| `error!`| Streaming loop failures (caught in async spawn)                              |

### Design Principle
- Persistence failures (`warn!`) are **non-blocking** — the agent loop continues even if DB writes fail.
- Tool/permission issues are warnings, not fatal errors.

**Key file:** `crates/krabs-core/src/agents/agent.rs`

---

### Hook-based Observability

Hooks form a structured event bus layered on top of raw logging.

**Events** (`crates/krabs-core/src/hooks/hook.rs`):
```
HookEvent::AgentStart        { task }
HookEvent::TurnStart         { turn }
HookEvent::TurnEnd           { turn }
HookEvent::PreToolUse        { tool_name, args, tool_use_id }
HookEvent::PostToolUse       { tool_name, args, result, tool_use_id }
HookEvent::PostToolUseFailure{ tool_name, args, error, tool_use_id }
HookEvent::AgentStop         { result }
```

**Registry** (`crates/krabs-core/src/hooks/registry.rs`):
- `HookRegistry::fire(&event)` — dispatches to all matching hooks async
- Tool name matching uses regex
- Resolution priority:
  - PreToolUse: `Deny > ModifyArgs > Allow`
  - Other events: `Stop > SystemMessage > AppendContext > Continue`
- Hook errors are logged and skipped (never fatal)

---

## 2. Durable Execution

### Storage Backend
- **SQLite** via `sqlx` v0.8 (`sqlite`, `runtime-tokio`, `migrate`, `macros` features)
- DB path configurable; resolved at `SessionStore::open(db_path)`
- Schema applied via sqlx migrations on open

**Key file:** `crates/krabs-core/src/session/session.rs`

---

### Schema

```
sessions
  id          TEXT PK       -- UUID
  agent_id    TEXT
  model       TEXT
  provider    TEXT
  created_at  INTEGER       -- Unix seconds

messages
  id          INTEGER PK AUTOINCREMENT  -- used as checkpoint boundary
  session_id  TEXT FK
  agent_id    TEXT
  turn        INTEGER
  role        TEXT          -- "user" | "assistant" | "tool"  (system never persisted)
  content     TEXT
  tool_call_id TEXT
  tool_name   TEXT
  tool_args   TEXT          -- JSON Vec<ToolCall> for assistant messages
  created_at  INTEGER

token_usage
  id, session_id, agent_id, turn
  input_tokens, output_tokens
  created_at  INTEGER

errors
  id, session_id, agent_id, turn
  context     TEXT          -- e.g. "llm_stream", "max_turns"
  message     TEXT
  attempt     INTEGER       -- 0-indexed retry count
  created_at  INTEGER

checkpoints
  id, session_id, agent_id, turn
  last_msg_id INTEGER       -- MAX(messages.id) at checkpoint time
  created_at  INTEGER
```

---

### Key Types

| Type               | Purpose                                                  |
|--------------------|----------------------------------------------------------|
| `SessionStore`     | Open DB, create/load sessions                           |
| `Session`          | Persist + reconstruct conversation state                 |
| `StoredMessage`    | Full DB record, reconstructed to provider `Message`      |
| `StoredCheckpoint` | Resume boundary via `last_msg_id`                        |
| `StoredError`      | Error + retry attempt for diagnostics                    |
| `StoredTokenUsage` | Per-turn token accounting                                |

---

### Checkpoint & Resume Flow

**Writing a checkpoint** (after each successful turn):
```
write_checkpoint(turn)
  → SELECT MAX(id) FROM messages WHERE session_id = ?
  → INSERT INTO checkpoints (turn, last_msg_id, ...)
```

**Resuming** (`KrabsAgent::build_async` with `ResumeMode::Resume`):
```
1. load_session(id)
2. latest_checkpoint()
3. If checkpoint:
     rollback_to(last_msg_id)   -- DELETE messages WHERE id > last_msg_id
     messages_up_to(last_msg_id) -- reload clean history
4. If no checkpoint:
     load all messages (best-effort)
5. Continue agent loop from recovered state
```

**Rollback** handles crash scenarios where the agent died mid-turn and left partial messages.

---

### Persistence During the Agent Loop

```
Each LLM message received     → persist_message(msg, turn)
Each LLM call completes       → persist_token_usage(turn, in, out)
Each retry failure            → persist_error(turn, context, error, attempt)
After each successful turn    → write_checkpoint(turn)
```

System messages are **never persisted** — they're rebuilt dynamically each turn, keeping the DB lean and allowing prompts to evolve.

---

### Retry with Persistence

`call_with_retry(turn, context, closure)`:
- Exponential backoff: base 500ms, multiplier `2^attempt`
- Each failure calls `persist_error(...)` with the attempt index
- Max retries: configurable (default 3)
- Failures are `warn!`, not `error!` — agent decides whether to abort

---

### CLI Resume

```bash
krabs --resume <session-id>
```

Parsed in `crates/krabs-cli/src/main.rs`; passed as `ResumeMode::Resume { session_id }` to `KrabsAgentBuilder`.

---

## 3. Architectural Decisions

| Decision | Rationale |
|---|---|
| Checkpoint by `last_msg_id` | Atomic, cheap reference; no distributed txn needed |
| System messages not persisted | Rebuilt each turn; allows dynamic prompts, reduces DB size |
| Persistence failures non-fatal | Agent liveness > perfect observability |
| Hook-based event bus | Decouples observability from business logic |
| Attempt tracking in errors | Enables cost/failure analysis per retry |
| `Result<T>` everywhere, no `unwrap` | Correctness by construction (CLAUDE.md law) |

---

## 4. Key Files

| File | Role |
|---|---|
| `crates/krabs-core/src/session/session.rs` | All persistence logic (~795 lines) |
| `crates/krabs-core/src/agents/agent.rs` | Agent loop + logging + checkpoint writes |
| `crates/krabs-core/src/hooks/hook.rs` | Event type definitions |
| `crates/krabs-core/src/hooks/registry.rs` | Hook dispatch & resolution |
| `crates/krabs-core/src/config/config.rs` | DB path config |
| `crates/krabs-cli/src/main.rs` | `--resume` CLI flag |
