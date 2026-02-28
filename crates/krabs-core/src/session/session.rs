use crate::providers::provider::{Message, Role, ToolCall};
#[cfg(test)]
use std::path::PathBuf;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── Schema ────────────────────────────────────────────────────────────────────

const MIGRATE: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT    PRIMARY KEY,
    agent_id    TEXT    NOT NULL,
    model       TEXT    NOT NULL,
    provider    TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    metadata    TEXT
);

CREATE TABLE IF NOT EXISTS messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT    NOT NULL REFERENCES sessions(id),
    agent_id     TEXT    NOT NULL,
    turn         INTEGER NOT NULL,
    role         TEXT    NOT NULL,
    content      TEXT    NOT NULL,
    tool_call_id TEXT,
    tool_name    TEXT,
    tool_args    TEXT,
    created_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS token_usage (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT    NOT NULL REFERENCES sessions(id),
    agent_id      TEXT    NOT NULL,
    turn          INTEGER NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS errors (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT    NOT NULL REFERENCES sessions(id),
    agent_id   TEXT    NOT NULL,
    turn       INTEGER NOT NULL,
    context    TEXT    NOT NULL,
    message    TEXT    NOT NULL,
    attempt    INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS checkpoints (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT    NOT NULL REFERENCES sessions(id),
    agent_id    TEXT    NOT NULL,
    turn        INTEGER NOT NULL,
    last_msg_id INTEGER NOT NULL,
    created_at  INTEGER NOT NULL
);
"#;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: i64,
    pub session_id: String,
    pub agent_id: String,
    pub turn: usize,
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    /// JSON-encoded tool call arguments (for assistant tool-call messages).
    pub tool_args: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredError {
    pub id: i64,
    pub session_id: String,
    pub agent_id: String,
    pub turn: usize,
    pub context: String,
    pub message: String,
    pub attempt: usize,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTokenUsage {
    pub id: i64,
    pub session_id: String,
    pub agent_id: String,
    pub turn: usize,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCheckpoint {
    pub id: i64,
    pub session_id: String,
    pub agent_id: String,
    pub turn: usize,
    /// The highest `messages.id` included in this checkpoint.
    pub last_msg_id: i64,
    pub created_at: i64,
}

// ── SessionStore ──────────────────────────────────────────────────────────────

pub struct SessionStore {
    pool: SqlitePool,
}

impl SessionStore {
    pub async fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let url = format!("sqlite://{}?mode=rwc", db_path.display());
        let pool = SqlitePool::connect(&url).await?;
        sqlx::query(MIGRATE).execute(&pool).await?;
        // Best-effort migration for existing DBs that pre-date the `attempt`
        // column and `checkpoints` table. SQLite returns an error if the column
        // already exists; we swallow it.
        let _ = sqlx::query("ALTER TABLE errors ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0")
            .execute(&pool)
            .await;
        Ok(Self { pool })
    }

    pub async fn new_session(
        &self,
        agent_id: &str,
        model: &str,
        provider: &str,
    ) -> Result<Arc<Session>> {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, model, provider, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(agent_id)
        .bind(model)
        .bind(provider)
        .bind(now_ts())
        .execute(&self.pool)
        .await?;

        Ok(Arc::new(Session {
            id,
            agent_id: agent_id.to_string(),
            pool: self.pool.clone(),
        }))
    }

    pub async fn load_session(&self, id: &str) -> Result<Arc<Session>> {
        let row = sqlx::query("SELECT agent_id FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Session {} not found", id))?;

        let agent_id: String = row.try_get("agent_id")?;

        Ok(Arc::new(Session {
            id: id.to_string(),
            agent_id,
            pool: self.pool.clone(),
        }))
    }
}

// ── Session ───────────────────────────────────────────────────────────────────

pub struct Session {
    pub id: String,
    pub agent_id: String,
    pool: SqlitePool,
}

impl Session {
    // ── Persistence ───────────────────────────────────────────────────────────

    /// Persist a message from the agent loop.
    ///
    /// System messages are **skipped** — they are ephemeral and rebuilt every
    /// turn from the current config/skills state.
    pub async fn persist_message(&self, message: &Message, turn: usize) -> Result<()> {
        if matches!(message.role, Role::System) {
            return Ok(());
        }

        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            Role::System => unreachable!(),
        };

        let tool_args = match &message.tool_calls {
            Some(calls) if !calls.is_empty() => Some(serde_json::to_string(calls)?),
            _ => None,
        };

        sqlx::query(
            "INSERT INTO messages \
             (session_id, agent_id, turn, role, content, tool_call_id, tool_name, tool_args, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&self.id)
        .bind(&self.agent_id)
        .bind(turn as i64)
        .bind(role)
        .bind(&message.content)
        .bind(&message.tool_call_id)
        .bind(&message.tool_name)
        .bind(&tool_args)
        .bind(now_ts())
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn persist_token_usage(
        &self,
        turn: usize,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO token_usage \
             (session_id, agent_id, turn, input_tokens, output_tokens, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&self.id)
        .bind(&self.agent_id)
        .bind(turn as i64)
        .bind(input_tokens as i64)
        .bind(output_tokens as i64)
        .bind(now_ts())
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Persist an error. `attempt` is 0-indexed (0 = first try, 1 = first retry, …).
    pub async fn persist_error(
        &self,
        turn: usize,
        context: &str,
        error: &anyhow::Error,
        attempt: usize,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO errors (session_id, agent_id, turn, context, message, attempt, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&self.id)
        .bind(&self.agent_id)
        .bind(turn as i64)
        .bind(context)
        .bind(format!("{error:#}"))
        .bind(attempt as i64)
        .bind(now_ts())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ── Checkpointing ─────────────────────────────────────────────────────────

    /// Write a checkpoint after a fully-completed turn.
    ///
    /// A checkpoint captures the highest `messages.id` at this moment, meaning
    /// all messages written up to this point are considered consistent and safe
    /// to resume from.
    pub async fn write_checkpoint(&self, turn: usize) -> Result<()> {
        let row = sqlx::query(
            "SELECT COALESCE(MAX(id), 0) as max_id FROM messages WHERE session_id = ?",
        )
        .bind(&self.id)
        .fetch_one(&self.pool)
        .await?;

        let last_msg_id: i64 = row.try_get("max_id")?;

        sqlx::query(
            "INSERT INTO checkpoints (session_id, agent_id, turn, last_msg_id, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&self.id)
        .bind(&self.agent_id)
        .bind(turn as i64)
        .bind(last_msg_id)
        .bind(now_ts())
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Load the most recent checkpoint for this session.
    pub async fn latest_checkpoint(&self) -> Result<Option<StoredCheckpoint>> {
        let row = sqlx::query(
            "SELECT id, session_id, agent_id, turn, last_msg_id, created_at \
             FROM checkpoints WHERE session_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(&self.id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|r| {
            Ok(StoredCheckpoint {
                id: r.try_get("id")?,
                session_id: r.try_get("session_id")?,
                agent_id: r.try_get("agent_id")?,
                turn: r.try_get::<i64, _>("turn")? as usize,
                last_msg_id: r.try_get("last_msg_id")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .transpose()
    }

    /// Load messages up to and including `last_msg_id` (the resume boundary).
    pub async fn messages_up_to(&self, last_msg_id: i64) -> Result<Vec<StoredMessage>> {
        let rows = sqlx::query(
            "SELECT id, session_id, agent_id, turn, role, content, \
                    tool_call_id, tool_name, tool_args, created_at \
             FROM messages WHERE session_id = ? AND id <= ? ORDER BY id ASC",
        )
        .bind(&self.id)
        .bind(last_msg_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| Self::row_to_stored(r)).collect()
    }

    /// Hard-delete messages written after `last_msg_id` (incomplete turn rollback).
    pub async fn rollback_to(&self, last_msg_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM messages WHERE session_id = ? AND id > ?")
            .bind(&self.id)
            .bind(last_msg_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ── Reconstruction ────────────────────────────────────────────────────────

    /// Convert a `StoredMessage` back into a provider `Message` for replay.
    ///
    /// System messages are never stored, so `role == "system"` is an error.
    pub fn stored_to_message(stored: &StoredMessage) -> Result<Message> {
        match stored.role.as_str() {
            "user" => Ok(Message::user(&stored.content)),
            "assistant" => {
                if stored.tool_args.is_some() {
                    let calls = Self::decode_tool_calls(stored)?;
                    Ok(Message::assistant_tool_calls(calls))
                } else {
                    Ok(Message::assistant(&stored.content))
                }
            }
            "tool" => {
                let call_id = stored.tool_call_id.as_deref().unwrap_or("");
                let tool_name = stored.tool_name.as_deref().unwrap_or("");
                Ok(Message::tool_result(&stored.content, call_id, tool_name))
            }
            other => anyhow::bail!("unexpected role in stored message: {other}"),
        }
    }

    /// Decode stored tool calls back into typed `ToolCall` values.
    pub fn decode_tool_calls(msg: &StoredMessage) -> Result<Vec<ToolCall>> {
        match &msg.tool_args {
            Some(json) => Ok(serde_json::from_str(json)?),
            None => Ok(Vec::new()),
        }
    }

    // ── Query helpers ─────────────────────────────────────────────────────────

    pub async fn messages(&self) -> Result<Vec<StoredMessage>> {
        let rows = sqlx::query(
            "SELECT id, session_id, agent_id, turn, role, content, \
                    tool_call_id, tool_name, tool_args, created_at \
             FROM messages WHERE session_id = ? ORDER BY id ASC",
        )
        .bind(&self.id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| Self::row_to_stored(r)).collect()
    }

    pub async fn search(&self, query: &str) -> Result<Vec<StoredMessage>> {
        let pattern = format!("%{}%", query);
        let rows = sqlx::query(
            "SELECT id, session_id, agent_id, turn, role, content, \
                    tool_call_id, tool_name, tool_args, created_at \
             FROM messages WHERE session_id = ? AND content LIKE ? ORDER BY id ASC",
        )
        .bind(&self.id)
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| Self::row_to_stored(r)).collect()
    }

    pub async fn token_usage(&self) -> Result<Vec<StoredTokenUsage>> {
        let rows = sqlx::query(
            "SELECT id, session_id, agent_id, turn, input_tokens, output_tokens, created_at \
             FROM token_usage WHERE session_id = ? ORDER BY turn ASC",
        )
        .bind(&self.id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(StoredTokenUsage {
                    id: r.try_get("id")?,
                    session_id: r.try_get("session_id")?,
                    agent_id: r.try_get("agent_id")?,
                    turn: r.try_get::<i64, _>("turn")? as usize,
                    input_tokens: r.try_get::<i64, _>("input_tokens")? as u32,
                    output_tokens: r.try_get::<i64, _>("output_tokens")? as u32,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect()
    }

    pub async fn errors(&self) -> Result<Vec<StoredError>> {
        let rows = sqlx::query(
            "SELECT id, session_id, agent_id, turn, context, message, attempt, created_at \
             FROM errors WHERE session_id = ? ORDER BY id ASC",
        )
        .bind(&self.id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(StoredError {
                    id: r.try_get("id")?,
                    session_id: r.try_get("session_id")?,
                    agent_id: r.try_get("agent_id")?,
                    turn: r.try_get::<i64, _>("turn")? as usize,
                    context: r.try_get("context")?,
                    message: r.try_get("message")?,
                    attempt: r.try_get::<i64, _>("attempt")? as usize,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect()
    }

    pub async fn total_token_usage(&self) -> Result<(u32, u32)> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(input_tokens), 0) as inp, \
                    COALESCE(SUM(output_tokens), 0) as out \
             FROM token_usage WHERE session_id = ?",
        )
        .bind(&self.id)
        .fetch_one(&self.pool)
        .await?;

        let inp: i64 = row.try_get("inp")?;
        let out: i64 = row.try_get("out")?;
        Ok((inp as u32, out as u32))
    }

    // ── Private ───────────────────────────────────────────────────────────────

    fn row_to_stored(r: sqlx::sqlite::SqliteRow) -> Result<StoredMessage> {
        Ok(StoredMessage {
            id: r.try_get("id")?,
            session_id: r.try_get("session_id")?,
            agent_id: r.try_get("agent_id")?,
            turn: r.try_get::<i64, _>("turn")? as usize,
            role: r.try_get("role")?,
            content: r.try_get("content")?,
            tool_call_id: r.try_get("tool_call_id")?,
            tool_name: r.try_get("tool_name")?,
            tool_args: r.try_get("tool_args")?,
            created_at: r.try_get("created_at")?,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn open_temp_store() -> (SessionStore, PathBuf) {
        let path = std::env::temp_dir().join(format!("krabs_test_{}.db", Uuid::new_v4()));
        let store = SessionStore::open(&path).await.expect("open store");
        (store, path)
    }

    #[tokio::test]
    async fn schema_is_created_on_open() {
        let (store, path) = open_temp_store().await;
        store
            .new_session("agent-1", "claude-sonnet-4-6", "anthropic")
            .await
            .expect("new session");
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn session_records_agent_metadata() {
        let (store, path) = open_temp_store().await;
        let session = store
            .new_session("agent-abc", "gpt-4o", "openai")
            .await
            .expect("new session");

        assert_eq!(session.agent_id, "agent-abc");
        assert!(!session.id.is_empty());

        let reloaded = store.load_session(&session.id).await.expect("load session");
        assert_eq!(reloaded.id, session.id);
        assert_eq!(reloaded.agent_id, "agent-abc");

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn persist_and_read_back_full_conversation() {
        let (store, path) = open_temp_store().await;
        let session = store
            .new_session("agent-1", "claude-sonnet-4-6", "anthropic")
            .await
            .expect("new session");

        session
            .persist_message(&Message::user("list files in /tmp"), 0)
            .await
            .unwrap();

        let tool_call = ToolCall {
            id: "call_001".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({ "command": "ls /tmp" }),
            thought_signature: None,
        };
        session
            .persist_message(&Message::assistant_tool_calls(vec![tool_call.clone()]), 1)
            .await
            .unwrap();
        session
            .persist_message(
                &Message::tool_result("file_a.txt\nfile_b.txt", &tool_call.id, &tool_call.name),
                1,
            )
            .await
            .unwrap();
        session
            .persist_message(
                &Message::assistant("The files in /tmp are: file_a.txt, file_b.txt."),
                2,
            )
            .await
            .unwrap();

        session.persist_token_usage(1, 120, 45).await.unwrap();
        session.persist_token_usage(2, 180, 30).await.unwrap();

        let messages = session.messages().await.unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
        let decoded = Session::decode_tool_calls(&messages[1]).unwrap();
        assert_eq!(decoded[0].name, "bash");
        assert_eq!(messages[2].role, "tool");
        assert_eq!(messages[2].tool_name.as_deref(), Some("bash"));
        assert_eq!(messages[3].role, "assistant");

        let (total_in, total_out) = session.total_token_usage().await.unwrap();
        assert_eq!(total_in, 300);
        assert_eq!(total_out, 75);

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn system_messages_are_not_persisted() {
        let (store, path) = open_temp_store().await;
        let session = store
            .new_session("agent-1", "gpt-4o", "openai")
            .await
            .unwrap();

        session
            .persist_message(&Message::system("You are helpful."), 0)
            .await
            .unwrap();
        session
            .persist_message(&Message::user("hello"), 0)
            .await
            .unwrap();

        let messages = session.messages().await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn persist_and_read_back_errors_with_attempt() {
        let (store, path) = open_temp_store().await;
        let session = store
            .new_session("agent-1", "claude-sonnet-4-6", "anthropic")
            .await
            .unwrap();

        let e = anyhow::anyhow!("HTTP 429: rate limit exceeded");
        session.persist_error(2, "llm_stream", &e, 0).await.unwrap();
        session.persist_error(2, "llm_stream", &e, 1).await.unwrap();
        session.persist_error(2, "llm_stream", &e, 2).await.unwrap();

        let errors = session.errors().await.unwrap();
        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].attempt, 0);
        assert_eq!(errors[1].attempt, 1);
        assert_eq!(errors[2].attempt, 2);
        assert!(errors[0].message.contains("rate limit"));

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn checkpoint_write_and_rollback() {
        let (store, path) = open_temp_store().await;
        let session = store
            .new_session("agent-1", "gpt-4o", "openai")
            .await
            .unwrap();

        // Turn 0: user + assistant (complete)
        session.persist_message(&Message::user("hello"), 0).await.unwrap();
        session.persist_message(&Message::assistant("hi!"), 0).await.unwrap();
        session.write_checkpoint(0).await.unwrap();

        let cp = session.latest_checkpoint().await.unwrap().expect("checkpoint exists");
        assert_eq!(cp.turn, 0);
        let checkpoint_msg_id = cp.last_msg_id;

        // Turn 1: partial — only the tool call was written, process dies here
        let call = ToolCall {
            id: "call_001".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({ "command": "ls" }),
            thought_signature: None,
        };
        session
            .persist_message(&Message::assistant_tool_calls(vec![call]), 1)
            .await
            .unwrap();

        // Before rollback: 3 messages
        assert_eq!(session.messages().await.unwrap().len(), 3);

        // Rollback to checkpoint boundary
        session.rollback_to(checkpoint_msg_id).await.unwrap();

        // After rollback: only 2 messages (the partial turn is gone)
        let after = session.messages().await.unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[1].role, "assistant");
        assert_eq!(after[1].content, "hi!");

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn stored_to_message_roundtrip() {
        let (store, path) = open_temp_store().await;
        let session = store
            .new_session("agent-1", "gpt-4o", "openai")
            .await
            .unwrap();

        let call = ToolCall {
            id: "t1".to_string(),
            name: "read".to_string(),
            args: serde_json::json!({ "path": "/tmp/x" }),
            thought_signature: None,
        };
        session.persist_message(&Message::user("hello"), 0).await.unwrap();
        session
            .persist_message(&Message::assistant_tool_calls(vec![call.clone()]), 1)
            .await
            .unwrap();
        session
            .persist_message(&Message::tool_result("content", &call.id, &call.name), 1)
            .await
            .unwrap();
        session
            .persist_message(&Message::assistant("done"), 2)
            .await
            .unwrap();

        let stored = session.messages().await.unwrap();
        let reconstructed: Vec<Message> = stored
            .iter()
            .map(|s| Session::stored_to_message(s).unwrap())
            .collect();

        assert!(matches!(reconstructed[0].role, Role::User));
        assert!(matches!(reconstructed[1].role, Role::Assistant));
        assert!(reconstructed[1].tool_calls.is_some());
        assert_eq!(reconstructed[1].tool_calls.as_ref().unwrap()[0].name, "read");
        assert!(matches!(reconstructed[2].role, Role::Tool));
        assert_eq!(reconstructed[2].tool_call_id.as_deref(), Some("t1"));
        assert!(matches!(reconstructed[3].role, Role::Assistant));
        assert_eq!(reconstructed[3].content, "done");

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn search_filters_by_content() {
        let (store, path) = open_temp_store().await;
        let session = store
            .new_session("agent-1", "gpt-4o", "openai")
            .await
            .unwrap();

        session.persist_message(&Message::user("tell me about Rust"), 0).await.unwrap();
        session.persist_message(&Message::assistant("Rust is a systems language."), 1).await.unwrap();
        session.persist_message(&Message::user("what about Python?"), 2).await.unwrap();

        assert_eq!(session.search("Rust").await.unwrap().len(), 2);
        assert_eq!(session.search("Python").await.unwrap().len(), 1);

        drop(store);
        let _ = std::fs::remove_file(path);
    }
}
