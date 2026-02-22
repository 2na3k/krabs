use crate::providers::provider::{Message, Role};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub struct Session {
    pub id: String,
    pool: SqlitePool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub created_at: i64,
}

pub struct SessionStore {
    pool: SqlitePool,
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl SessionStore {
    pub async fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let url = format!("sqlite://{}?mode=rwc", db_path.display());
        let pool = SqlitePool::connect(&url).await?;
        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    async fn migrate(pool: &SqlitePool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                created_at INTEGER NOT NULL,
                metadata TEXT
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                tool_call_id TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS token_usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                turn INTEGER NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            "#,
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn new_session(&self) -> Result<Session> {
        let id = Uuid::new_v4().to_string();
        let ts = now_ts();
        sqlx::query("INSERT INTO sessions (id, created_at) VALUES (?, ?)")
            .bind(&id)
            .bind(ts)
            .execute(&self.pool)
            .await?;
        Ok(Session {
            id,
            pool: self.pool.clone(),
        })
    }

    pub async fn load_session(&self, id: &str) -> Result<Session> {
        let exists: bool = sqlx::query("SELECT 1 FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .is_some();
        if !exists {
            anyhow::bail!("Session {} not found", id);
        }
        Ok(Session {
            id: id.to_string(),
            pool: self.pool.clone(),
        })
    }
}

impl Session {
    pub async fn add(&self, message: &Message) -> Result<()> {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let ts = now_ts();
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_call_id, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&self.id)
        .bind(role)
        .bind(&message.content)
        .bind(&message.tool_call_id)
        .bind(ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn messages(&self) -> Result<Vec<Message>> {
        let rows = sqlx::query(
            "SELECT role, content, tool_call_id FROM messages WHERE session_id = ? ORDER BY id ASC",
        )
        .bind(&self.id)
        .fetch_all(&self.pool)
        .await?;

        let mut msgs = Vec::new();
        for row in rows {
            let role_str: String = row.try_get("role")?;
            let role = match role_str.as_str() {
                "system" => Role::System,
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "tool" => Role::Tool,
                _ => Role::User,
            };
            msgs.push(Message {
                role,
                content: row.try_get("content")?,
                tool_call_id: row.try_get("tool_call_id")?,
            });
        }
        Ok(msgs)
    }

    pub async fn search(&self, query: &str) -> Result<Vec<Message>> {
        let pattern = format!("%{}%", query);
        let rows = sqlx::query(
            "SELECT role, content, tool_call_id FROM messages WHERE session_id = ? AND content LIKE ? ORDER BY id ASC",
        )
        .bind(&self.id)
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await?;

        let mut msgs = Vec::new();
        for row in rows {
            let role_str: String = row.try_get("role")?;
            let role = match role_str.as_str() {
                "system" => Role::System,
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "tool" => Role::Tool,
                _ => Role::User,
            };
            msgs.push(Message {
                role,
                content: row.try_get("content")?,
                tool_call_id: row.try_get("tool_call_id")?,
            });
        }
        Ok(msgs)
    }

    pub async fn record_token_usage(
        &self,
        turn: usize,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<()> {
        let ts = now_ts();
        sqlx::query(
            "INSERT INTO token_usage (session_id, turn, input_tokens, output_tokens, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&self.id)
        .bind(turn as i64)
        .bind(input_tokens as i64)
        .bind(output_tokens as i64)
        .bind(ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn total_token_usage(&self) -> Result<(u32, u32)> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(input_tokens), 0) as inp, COALESCE(SUM(output_tokens), 0) as out FROM token_usage WHERE session_id = ?",
        )
        .bind(&self.id)
        .fetch_one(&self.pool)
        .await?;
        let inp: i64 = row.try_get("inp")?;
        let out: i64 = row.try_get("out")?;
        Ok((inp as u32, out as u32))
    }
}
