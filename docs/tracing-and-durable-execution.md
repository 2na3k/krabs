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
- Retry warnings are also forwarded to the CLI as `StreamChunk::Status` messages (rendered as dimmed italic lines), so users see retries in real time without needing a log subscriber.

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

> `PostToolUseFailure` fires when the final `ToolResult` has `is_error: true` (both hard errors
> normalised by `call_tool_with_retry` and soft tool errors after retries are exhausted).
> `PostToolUse` fires only on success. Hooks no longer need to inspect `result.is_error`.

---

### TelemetryHook — Raw Event Export

`crates/krabs-core/src/hooks/telemetry.rs`

Exports every `HookEvent` as a JSON envelope to up to three backends simultaneously (all fire-and-forget):

| Backend | How to enable |
|---|---|
| HTTP POST | Set `telemetry.http_endpoint` in config |
| JSONL file | Set `telemetry.jsonl_path` (auto-defaults to `/tmp/krabs-telemetry-<session_id>.jsonl`) |
| mpsc channel | Programmatic: `.channel(tx)` on `TelemetryHookBuilder` |

**Envelope shape:**
```json
{
  "event_type": "pre_tool_use",
  "timestamp_ms": 1740787200123,
  "session_id": "abc-123",
  "agent_id": "agent-xyz",
  "payload": { ... }
}
```

**Auto-wired via config** (`config.telemetry.enabled = true`): `build_async` registers it automatically with the session ID and agent ID already set.

**Programmatic use:**
```rust
let hook = TelemetryHookBuilder::new()
    .http_endpoint("http://localhost:9000/events")
    .jsonl_path("/tmp/my-agent.jsonl")
    .channel(tx)
    .build();
agent_builder.hook(Arc::new(hook));
```

---

### LangfuseHook — Structured Tracing

`crates/krabs-core/src/hooks/langfuse.rs`

Maps agent lifecycle events to the [Langfuse](https://langfuse.com) batch ingestion API (`POST /api/public/ingestion`), producing a fully nested trace in the Langfuse UI.

**Event mapping:**

| HookEvent | Langfuse type | Effect |
|---|---|---|
| `AgentStart` | `trace-create` | Creates root trace with `input=task` |
| `TurnStart` | `span-create` | Child span of trace |
| `PreToolUse` | `span-create` | Child of current turn span, `input=args` |
| `PostToolUse` | `span-update` | Closes tool span with `output=result` |
| `PostToolUseFailure` | `span-update` | Closes tool span with `level=ERROR` |
| `TurnEnd` | `span-update` | Closes turn span with `endTime` |
| `AgentStop` | `trace-create` (upsert) | Adds `output=result` to root trace |

**Resulting trace shape in Langfuse:**
```
Trace: "agent-run"  (input=task, output=final result)
  └─ Span: "turn-0"
       └─ Span: "bash"       input={args}  output=result
       └─ Span: "read_file"  input={args}  level=ERROR
  └─ Span: "turn-1"
       └─ Span: "web_fetch"  input={args}  output=result
```

**Auto-wired via config:**
```json
{
  "langfuse": {
    "enabled": true,
    "public_key": "pk-lf-...",
    "secret_key": "sk-lf-...",
    "base_url": "http://localhost:3000"
  }
}
```

**Programmatic use:**
```rust
let hook = LangfuseHookBuilder::new("pk-lf-...", "sk-lf-...")
    .base_url("http://localhost:3000")
    .session_id("my-session")
    .agent_id("my-agent")
    .build();
agent_builder.hook(Arc::new(hook));
```

**Local Langfuse stack:** `docker-compose.yml` at the repo root spins up the full Langfuse v3 stack (Postgres, ClickHouse, MinIO, Redis). Run with `docker compose up -d` then open `http://localhost:3000`.


---

## 2. Durable Execution

### Storage Backend
- **SQLite** via `sqlx` v0.8 (`sqlite`, `runtime-tokio`, `migrate`, `macros` features)
- DB path configurable; resolved at `SessionStore::open(db_path)`
- Schema applied inline via `sqlx::query(MIGRATE)` on open (not via sqlx migration files)
- Best-effort `ALTER TABLE errors ADD COLUMN attempt` runs on open to upgrade pre-existing DBs that lack the column; errors are silently swallowed

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
  metadata    TEXT          -- reserved, currently unused

messages
  id          INTEGER PK AUTOINCREMENT  -- used as checkpoint boundary
  session_id  TEXT FK
  agent_id    TEXT
  turn        INTEGER
  role        TEXT          -- "system" | "user" | "assistant" | "tool"
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
  context     TEXT          -- e.g. "llm_stream", "llm_complete", "bash", "max_turns"
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

**Additional `Session` query helpers** (not used by the agent loop directly):
- `session.search(query)` — LIKE search over message content
- `session.total_token_usage()` — aggregate `SUM(input_tokens), SUM(output_tokens)`

---

### Checkpoint & Resume Flow

**Writing a checkpoint** (after each successful turn):
```
write_checkpoint(turn)
  → SELECT COALESCE(MAX(id), 0) FROM messages WHERE session_id = ?
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
Newest user message submitted  → persist_message(msg, turn=0)   [streaming path only]
Each LLM message received      → persist_message(msg, turn)
Each LLM call completes        → persist_token_usage(turn, in, out)
Each retry failure             → persist_error(turn, context, error, attempt)
After each successful turn     → write_checkpoint(turn)
```

> **Streaming path detail:** `streaming_loop_inner` persists the **last** (newest) user
> message in the history at startup — not the first. This correctly handles resumed sessions
> where earlier user messages are already in the DB.

System messages **are persisted** alongside other roles. On resume, the full conversation including system messages is reloaded from the DB.

---

### LLM Retry with Persistence

`call_with_retry(turn, context, status_tx, closure)`:
- Exponential backoff: `base_ms * 2^attempt` (base 500ms default)
- Each failure calls `persist_error(turn, context, error, attempt)` with the 0-indexed attempt
- Max retries: `config.max_retries` (default 3, so 4 total attempts)
- Each retry emits a `StreamChunk::Status` via `status_tx` (streaming path) so the CLI shows retry progress
- Failures are `warn!`, not `error!` — after exhausting retries the `Err` propagates and the agent loop decides whether to abort

---

### Tool Retry with Persistence

`call_tool_with_retry(turn, tool_name, tool, args, status_tx)`:
- Handles **both** hard errors (`Err(e)`) and soft errors (`ToolResult { is_error: true }`)
- Exponential backoff reuses `config.retry_base_delay_ms`
- Max retries: `config.tool_max_retries` (default 1, so 2 total attempts)
- Hard errors are persisted via `persist_error`; soft errors are not (they're a tool-level concern)
- Each retry emits a `StreamChunk::Status` to the CLI
- After exhausting retries, returns the final `ToolResult` to the LLM; `PostToolUse` fires for all outcomes

---

### CLI Session Continuity

The CLI maintains session continuity across turns and queued messages:

```
Turn completes (Done)  → DisplayEvent::Done { messages, session_id }
                         active_resume_id = session_id
Turn fails (Error)     → DisplayEvent::Error { message, session_id }
                         active_resume_id = session_id

Queued message dispatched → build_agent(..., active_resume_id.take())
                            → ResumeMode::Resume { session_id }
                            → same session continues in DB
```

This ensures that a message typed while the agent is thinking, and dispatched after the current turn completes (or errors), is recorded in the same session rather than creating a new one.

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
| All roles persisted (incl. system) | Full conversation state in DB enables exact replay on resume |
| Persistence failures non-fatal | Agent liveness > perfect observability |
| Hook-based event bus | Decouples observability from business logic |
| Attempt tracking in errors | Enables cost/failure analysis per retry |
| `status_tx` in retry helpers | Retry visibility in CLI without requiring a log subscriber |
| `PostToolUseFailure` on error, `PostToolUse` on success | Clean hook surface; hooks don't need to inspect `is_error` |
| `Result<T>` everywhere, no `unwrap` | Correctness by construction (CLAUDE.md law) |

---

## 4. Key Files

| File | Role |
|---|---|
| `crates/krabs-core/src/session/session.rs` | All persistence logic (~800 lines) |
| `crates/krabs-core/src/agents/agent.rs` | Agent loop + logging + checkpoint writes + retry helpers |
| `crates/krabs-core/src/hooks/hook.rs` | Event type definitions |
| `crates/krabs-core/src/hooks/registry.rs` | Hook dispatch & resolution |
| `crates/krabs-core/src/hooks/telemetry.rs` | Raw event export (HTTP / JSONL / channel) |
| `crates/krabs-core/src/hooks/langfuse.rs` | Langfuse trace/span mapping |
| `crates/krabs-core/src/config/config.rs` | DB path, retry, telemetry, and langfuse config |
| `crates/krabs-core/examples/langfuse_trace.rs` | Langfuse smoke-test example |
| `docker-compose.yml` | Local Langfuse v3 stack |
| `crates/krabs-core/src/providers/provider.rs` | `StreamChunk::Status` for retry visibility |
| `crates/krabs-cli/src/main.rs` | `--resume` CLI flag |
| `crates/krabs-cli/src/chat/agent.rs` | Session ID threading through `DisplayEvent` |
