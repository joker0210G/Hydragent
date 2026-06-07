use sqlx::{SqlitePool, Row};
use std::path::Path;
use hydragent_types::{Message, MessageRole};
use anyhow::Result;

pub struct SessionStore {
    pool: SqlitePool,
}

impl SessionStore {
    pub async fn new(database_url: &str) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = Path::new(database_url).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let pool = SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(database_url)
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        ).await?;

        let store = Self { pool };
        store.init_db().await?;

        Ok(store)
    }

    async fn init_db(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id   TEXT    NOT NULL,
                role         TEXT    NOT NULL,
                content      TEXT    NOT NULL,
                token_count  INTEGER,
                timestamp    INTEGER NOT NULL
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS tool_calls (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id    TEXT    NOT NULL,
                call_id       TEXT    NOT NULL UNIQUE,
                tool_id       TEXT    NOT NULL,
                params_hash   TEXT    NOT NULL,
                status        TEXT    NOT NULL CHECK(status IN ('success','failure','timeout')),
                execution_ms  INTEGER NOT NULL,
                timestamp     INTEGER NOT NULL
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS session_meta (
                session_id    TEXT    PRIMARY KEY,
                created_at    INTEGER NOT NULL,
                last_active   INTEGER NOT NULL,
                turn_count    INTEGER NOT NULL DEFAULT 0,
                model_used    TEXT
            );"
        ).execute(&self.pool).await?;

        // Create indexes
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, timestamp);")
            .execute(&self.pool).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tool_calls_session ON tool_calls(session_id);")
            .execute(&self.pool).await?;

        Ok(())
    }

    pub async fn create_session(&self, session_id: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT OR IGNORE INTO session_meta (session_id, created_at, last_active, turn_count)
             VALUES (?, ?, ?, 0)"
        )
        .bind(session_id)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn append_message(&self, session_id: &str, role: MessageRole, content: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let role_str = match role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
        };

        sqlx::query(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES (?, ?, ?, ?)"
        )
        .bind(session_id)
        .bind(role_str)
        .bind(content)
        .bind(now)
        .execute(&self.pool)
        .await?;

        // Update session meta
        sqlx::query(
            "UPDATE session_meta
             SET last_active = ?, turn_count = turn_count + 1
             WHERE session_id = ?"
        )
        .bind(now)
        .bind(session_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn load_recent(&self, session_id: &str, limit: u32) -> Result<Vec<Message>> {
        let rows = sqlx::query_as::<_, Message>(
            "SELECT id, session_id, role, content, token_count, timestamp
             FROM messages
             WHERE session_id = ?
             ORDER BY timestamp ASC
             LIMIT ?"
        )
        .bind(session_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn list_sessions(&self) -> Result<Vec<(String, i64, i64, i32)>> {
        let rows = sqlx::query(
            "SELECT session_id, created_at, last_active, turn_count
             FROM session_meta
             ORDER BY last_active DESC"
        )
        .fetch_all(&self.pool)
        .await?;

        let mut list = Vec::new();
        for row in rows {
            let session_id: String = row.get("session_id");
            let created_at: i64 = row.get("created_at");
            let last_active: i64 = row.get("last_active");
            let turn_count: i32 = row.get("turn_count");
            list.push((session_id, created_at, last_active, turn_count));
        }

        Ok(list)
    }
}
