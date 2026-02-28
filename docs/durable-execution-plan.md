# Durable Execution — Session Recovery Plan

## What we're building

Three interlocking features that together give Krabs durable execution semantics:

1. **Checkpointing** — write a stable marker after each completed turn
2. **Retry with backoff** — automatically retry failed LLM API calls before giving up
3. **Session resume** — reconstruct conversation state from the DB and continue from the last checkpoint

CLI surface: `krabs --resume <session-id>` on startup, `/resume <session-id>` slash command mid-session.

---

## Schema changes

### New table: `checkpoints`

```sql
CREATE TABLE IF NOT EXISTS checkpoints (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT    NOT NULL REFERENCES sessions(id),
    agent_id     TEXT    NOT NULL,
    turn         INTEGER NOT NULL,
    last_msg_id  INTEGER NOT NULL,  -- max messages.id at time of checkpoint
    created_at   INTEGER NOT NULL
);
```

A checkpoint row means: *all messages with `id <= last_msg_id` are complete and consistent for this session up to `turn`.* On resume, messages written after `last_msg_id` are hard-deleted (incomplete turn rollback), then the agent loop restarts from that state.

### Amend `errors` table

```sql
ALTER TABLE errors ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0;
```

Allows multiple error rows per `(session_id, turn, context)` with an incrementing `attempt` counter to track retries.

---

## Config changes (`config.rs`)

Two new serde-defaulted fields in `KrabsConfig`:

```rust
#[serde(default = "default_max_retries")]
pub max_retries: usize,           // default: 3

#[serde(default = "default_retry_base_delay_ms")]
pub retry_base_delay_ms: u64,     // default: 500ms
```

Backward-compatible — existing `config.json` files without these keys get the defaults.

---

## New structs

### `StoredCheckpoint` (session.rs)

```rust
pub struct StoredCheckpoint {
    pub id: i64,
    pub session_id: String,
    pub agent_id: String,
    pub turn: usize,
    pub last_msg_id: i64,
    pub created_at: i64,
}
```

### `ResumeMode` (agent.rs, builder-internal)

```rust
enum ResumeMode {
    New,
    Resume { session_id: String },
}
```

Held on `KrabsAgentBuilder`, consumed in `build_async`. Not exposed on `KrabsAgent` or the `Agent` trait — no breaking changes.

---

## New methods on `Session` (session.rs)

```rust
// Write a checkpoint after a fully-completed turn
pub async fn write_checkpoint(&self, turn: usize) -> Result<()>

// Load the most recent checkpoint for this session
pub async fn latest_checkpoint(&self) -> Result<Option<StoredCheckpoint>>

// Load messages up to and including last_msg_id (resume point)
pub async fn messages_up_to(&self, last_msg_id: i64) -> Result<Vec<StoredMessage>>

// Hard-delete messages written after last_msg_id (incomplete turn rollback)
pub async fn rollback_to(&self, last_msg_id: i64) -> Result<()>

// Convert a StoredMessage back to a provider Message
pub fn stored_to_message(stored: &StoredMessage) -> Result<Message>
```

`stored_to_message` is the inverse of `persist_message`. It lives in `session.rs` alongside `decode_tool_calls`:

| `role` | `tool_args` | Result |
|---|---|---|
| `user` | — | `Message::user(&content)` |
| `assistant` | `Some(json)` | `Message::assistant_tool_calls(decode_tool_calls?)` |
| `assistant` | `None` | `Message::assistant(&content)` |
| `tool` | — | `Message::tool_result(&content, tool_call_id, tool_name)` |
| `system` | — | never stored → error |

---

## Changes to `KrabsAgent` (agent.rs)

### 1. Builder resume support

```rust
impl KrabsAgentBuilder {
    pub fn resume_session(mut self, session_id: impl Into<String>) -> Self {
        self.resume_mode = ResumeMode::Resume { session_id: session_id.into() };
        self
    }
}
```

`build_async` with `ResumeMode::Resume` calls `store.load_session(&id)` instead of `store.new_session(...)`. The agent gets the existing session so all new messages go under the original session ID.

### 2. History reconstruction

```rust
pub async fn load_history_from_session(&self) -> Result<Vec<Message>>
```

Logic:
1. Load `latest_checkpoint()`.
2. If checkpoint exists: call `rollback_to(last_msg_id)` then `messages_up_to(last_msg_id)`.
3. If no checkpoint: `session.messages()` (best-effort — no incomplete-turn rollback).
4. Convert each `StoredMessage` → `Message` via `stored_to_message`.
5. Return the `Vec<Message>`. The caller prepends a system message before passing to the loop.

### 3. Retry with exponential backoff

```rust
async fn call_with_retry<F, Fut, T>(
    &self,
    turn: usize,
    context: &str,
    f: impl FnMut() -> Fut,
) -> Result<T>
where Fut: Future<Output = Result<T>>
```

- Loops `0..=max_retries`.
- On success → return `Ok`.
- On error → `persist_error(turn, context, &e, attempt)`.
- If `attempt < max_retries` → `tokio::time::sleep(base_delay_ms * 2^attempt)`.
- After all attempts exhausted → `Err(last_error)`.

Wraps both `stream_complete` (in `streaming_loop_inner`) and `complete` (in `run()`).

### 4. Checkpoint write points

After each fully-completed turn in both loop paths:
- **Tool-call branch**: after all tool results for the turn are persisted.
- **Final message branch**: after the final assistant message is persisted.

---

## CLI changes

### `main.rs`

```rust
let resume_id = std::env::args()
    .collect::<Vec<_>>()
    .windows(2)
    .find(|w| w[0] == "--resume")
    .map(|w| w[1].clone());

chat::run(creds, resume_id).await
```

### `chat.rs`

1. `pub async fn run(creds, resume_id: Option<String>)` — on startup, if `resume_id` is set, call `load_resume_history` and pre-populate both `messages` and the display.

2. `/resume <session-id>` slash command — mid-session resume. Clears current state and reloads the target session.

3. `build_agent` gains a `resume_session_id: Option<String>` parameter. When set, calls `.resume_session(sid)` on the builder.

---

## Invariants

- **Tool calls are not idempotent by the framework** — the framework does not track which tool calls in a partial turn already ran. Rolling back to the last checkpoint means the whole turn re-executes. Tool idempotency is the tool author's responsibility.
- **Rollback is hard-delete** — messages after the last checkpoint are removed permanently on resume. Post-mortem inspection of partial turns is handled by the `errors` table and `tracing` logs.
- **Retry targets only LLM API calls** — tool errors become tool result messages that the LLM reasons about. HTTP 429 / 503 from the provider are the primary retry target.
- **No `Agent` trait changes** — `resume_session` is a builder option, `load_history_from_session` is a `KrabsAgent` method. The `Agent` trait (`run(&self, task) -> Result<AgentOutput>`) is untouched.

---

## Implementation order

1. `session.rs` — schema + `MIGRATE_V2`, `StoredCheckpoint`, `stored_to_message`, `write_checkpoint`, `rollback_to`, `messages_up_to`, `latest_checkpoint`, amend `persist_error` for `attempt`
2. `config.rs` — `max_retries`, `retry_base_delay_ms`
3. `agent.rs` — `call_with_retry`, checkpoint write calls, `ResumeMode` on builder, `load_history_from_session`
4. `lib.rs` — export `Session`, `SessionStore`, `StoredCheckpoint`, `StoredMessage`
5. `main.rs` — parse `--resume`
6. `chat.rs` — `run` signature, `load_resume_history`, `/resume` slash command, amended `build_agent`
