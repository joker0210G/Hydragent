use sqlx::{SqlitePool, Row};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use hydragent_types::{Message, MessageRole};
use anyhow::Result;
use hydragent_embed::{LocalEmbedder, ensure_model_downloaded};
use crate::vector_index::VectorStore;
use serde_json::{json, Value};

pub struct SessionStore {
    pool: SqlitePool,
    pub(crate) data_dir: String,
    pub(crate) vector_store_path: PathBuf,
    pub(crate) vector_store: Mutex<VectorStore>,
    pub(crate) embedder: tokio::sync::OnceCell<LocalEmbedder>,
    /// Soft cap on the `semantic_memories` table size. When an insert
    /// pushes the count above this, the oldest+least-important rows are
    /// evicted. Default is `usize::MAX` (unbounded). Set via
    /// [`SessionStore::with_max_memories`] (called from `main.rs` based
    /// on the `MAX_SEMANTIC_MEMORIES` env var / `AppConfig`).
    pub(crate) max_memories: usize,
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
                .pragma("foreign_keys", "on")
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
                .busy_timeout(std::time::Duration::from_millis(5000))
        ).await?;

        let data_dir = Path::new(database_url).parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "./data".to_string());

        let vector_store_path = Path::new(&data_dir).join("vectors.bin");
        let vector_store = if vector_store_path.exists() {
            VectorStore::load_from_disk(&vector_store_path).unwrap_or_else(|_| VectorStore::new())
        } else {
            VectorStore::new()
        };

        let store = Self {
            pool,
            data_dir,
            vector_store_path,
            vector_store: Mutex::new(vector_store),
            embedder: tokio::sync::OnceCell::new(),
            max_memories: usize::MAX,
        };
        store.init_db().await?;

        Ok(store)
    }

    /// Run an `ALTER TABLE … ADD COLUMN` while tolerating "duplicate column"
    /// errors. Used for idempotent schema migrations: on a fresh DB the
    /// `CREATE TABLE IF NOT EXISTS` ran with the column already, and on an
    /// upgraded DB the column already exists from a previous boot.
    async fn try_alter(pool: &SqlitePool, sql: &str) {
        match sqlx::query(sql).execute(pool).await {
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("duplicate column") || msg.contains("already exists") {
                    // Expected when upgrading an already-migrated DB.
                } else {
                    tracing::warn!("migration step `{}` failed: {}", sql, e);
                }
            }
        }
    }

    /// Create an index, tolerating "already exists".
    async fn try_index(pool: &SqlitePool, sql: &str) {
        if let Err(e) = sqlx::query(sql).execute(pool).await {
            tracing::warn!("index step `{}` failed: {}", sql, e);
        }
    }

    pub async fn get_embedder(&self) -> Result<&LocalEmbedder> {
        self.embedder.get_or_try_init(|| async {
            let paths = ensure_model_downloaded(&self.data_dir).await?;
            let embedder = LocalEmbedder::new(&paths.model_path, &paths.tokenizer_path)?;
            Ok(embedder)
        }).await
    }

    /// Public accessor for the in-memory vector store. The `Mutex`
    /// guarantees safe concurrent access, so callers can hold a lock
    /// guard and call `insert` / `search` / `clear` / `delete` directly.
    ///
    /// Exposed primarily so the Criterion retrieval benchmark can
    /// drive raw cosine scans independently of the `hybrid_search` RRF
    /// path. Application code should prefer [`Self::insert_memory`]
    /// and [`crate::hybrid_search`].
    pub fn vector_store(&self) -> &Mutex<VectorStore> {
        &self.vector_store
    }

    async fn init_db(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                page_id      TEXT    NOT NULL,
                role         TEXT    NOT NULL,
                content      TEXT    NOT NULL,
                token_count  INTEGER,
                timestamp    INTEGER NOT NULL
            );"
        ).execute(&self.pool).await?;

        // Add columns if they don't exist
        let _ = sqlx::query("ALTER TABLE messages ADD COLUMN chunk_id TEXT;").execute(&self.pool).await;
        let _ = sqlx::query("ALTER TABLE messages ADD COLUMN requires_consolidation BOOLEAN DEFAULT 1;").execute(&self.pool).await;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS tool_calls (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                page_id       TEXT    NOT NULL,
                call_id       TEXT    NOT NULL UNIQUE,
                tool_id       TEXT    NOT NULL,
                params_hash   TEXT    NOT NULL,
                status        TEXT    NOT NULL CHECK(status IN ('success','failure','timeout')),
                execution_ms  INTEGER NOT NULL,
                timestamp     INTEGER NOT NULL
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS page_meta (
                page_id       TEXT    PRIMARY KEY,
                created_at    INTEGER NOT NULL,
                last_active   INTEGER NOT NULL,
                turn_count    INTEGER NOT NULL DEFAULT 0,
                model_used    TEXT,
                summary       TEXT
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS semantic_memories (
                id          TEXT    PRIMARY KEY,
                page_id     TEXT,
                content     TEXT    NOT NULL,
                importance  INTEGER NOT NULL DEFAULT 1,
                timestamp   INTEGER NOT NULL
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS user_insights (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                page_id     TEXT    NOT NULL,
                insight     TEXT    NOT NULL,
                timestamp   INTEGER NOT NULL
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS semantic_memories_fts USING fts5(
                id UNINDEXED,
                content
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS memory_consolidation_jobs (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                status      TEXT    NOT NULL CHECK(status IN ('pending', 'processing', 'completed', 'failed')),
                started_at  INTEGER,
                finished_at INTEGER
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS cron_jobs (
                id              TEXT    PRIMARY KEY,
                cron_expr       TEXT    NOT NULL,
                description     TEXT    NOT NULL,
                task_type       TEXT    NOT NULL DEFAULT 'react_loop',
                task_params     TEXT    NOT NULL DEFAULT '{}',
                target_channel_id TEXT  NOT NULL DEFAULT '*',
                status          TEXT    NOT NULL CHECK(status IN ('active', 'paused', 'deleted')),
                created_at      INTEGER NOT NULL,
                last_run_at     INTEGER,
                run_count       INTEGER NOT NULL DEFAULT 0
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS cron_job_runs (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id          TEXT    NOT NULL,
                started_at      INTEGER NOT NULL,
                completed_at    INTEGER,
                status          TEXT    NOT NULL CHECK(status IN ('running', 'completed', 'failed')),
                output_summary  TEXT,
                FOREIGN KEY(job_id) REFERENCES cron_jobs(id)
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS work_iq_feeds (
                url                     TEXT    PRIMARY KEY,
                name                    TEXT    NOT NULL,
                keywords                TEXT    NOT NULL,
                digest_channel          TEXT    NOT NULL,
                digest_cron             TEXT    NOT NULL,
                last_seen_id            TEXT
            );"
        ).execute(&self.pool).await?;

        // ---- Work IQ schema migrations (idempotent) ----
        // Older DBs (pre-v2) have a leaner schema. We add columns one at a time
        // and tolerate "duplicate column" errors so existing installs keep working.
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_feeds ADD COLUMN keywords_json TEXT").await;
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_feeds ADD COLUMN backfill_policy TEXT NOT NULL DEFAULT 'none'").await;
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_feeds ADD COLUMN backfill_n INTEGER NOT NULL DEFAULT 10").await;
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_feeds ADD COLUMN etag TEXT").await;
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_feeds ADD COLUMN last_modified TEXT").await;
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_feeds ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1").await;
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_feeds ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0").await;
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_feeds ADD COLUMN last_polled_at INTEGER").await;
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_feeds ADD COLUMN last_seen_published_at INTEGER").await;
        // Index for the "un-digested entries per feed" query and TTL sweeps.
        Self::try_index(&self.pool,
            "CREATE INDEX IF NOT EXISTS idx_work_iq_entries_feed_digested ON work_iq_entries(feed_url, digested)").await;
        Self::try_index(&self.pool,
            "CREATE INDEX IF NOT EXISTS idx_work_iq_entries_fetched_at ON work_iq_entries(fetched_at)").await;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS work_iq_entries (
                id              TEXT    PRIMARY KEY,
                feed_url        TEXT    NOT NULL,
                title           TEXT    NOT NULL,
                summary         TEXT    NOT NULL,
                url             TEXT    NOT NULL,
                fetched_at      INTEGER NOT NULL,
                published_at    INTEGER,
                digested        BOOLEAN NOT NULL DEFAULT 0,
                score           REAL    NOT NULL DEFAULT 0,
                FOREIGN KEY(feed_url) REFERENCES work_iq_feeds(url) ON DELETE CASCADE
            );"
        ).execute(&self.pool).await?;
        // published_at / score may be missing on old installs.
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_entries ADD COLUMN published_at INTEGER").await;
        Self::try_alter(&self.pool,
            "ALTER TABLE work_iq_entries ADD COLUMN score REAL NOT NULL DEFAULT 0").await;


        sqlx::query(
            "CREATE TABLE IF NOT EXISTS memory_tags (
                memory_id   TEXT    NOT NULL,
                tag         TEXT    NOT NULL,
                PRIMARY KEY (memory_id, tag),
                FOREIGN KEY (memory_id) REFERENCES semantic_memories(id) ON DELETE CASCADE
            );"
        ).execute(&self.pool).await?;

        // Triggers to sync to FTS virtual table
        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS fts_insert AFTER INSERT ON semantic_memories BEGIN
                INSERT INTO semantic_memories_fts (id, content) VALUES (new.id, new.content);
            END;"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS fts_update AFTER UPDATE ON semantic_memories BEGIN
                UPDATE semantic_memories_fts SET content = new.content WHERE id = new.id;
            END;"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS fts_delete AFTER DELETE ON semantic_memories BEGIN
                DELETE FROM semantic_memories_fts WHERE id = old.id;
            END;"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS nodes (
                node_id    TEXT    PRIMARY KEY,
                type       TEXT    NOT NULL,
                label      TEXT    NOT NULL,
                properties TEXT
            );"
        ).execute(&self.pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS edges (
                edge_id           TEXT    PRIMARY KEY,
                source_node_id    TEXT    NOT NULL,
                target_node_id    TEXT    NOT NULL,
                relation_type     TEXT    NOT NULL,
                weight            REAL    NOT NULL DEFAULT 1.0,
                FOREIGN KEY (source_node_id) REFERENCES nodes(node_id) ON DELETE CASCADE,
                FOREIGN KEY (target_node_id) REFERENCES nodes(node_id) ON DELETE CASCADE
            );"
        ).execute(&self.pool).await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_node_id);")
            .execute(&self.pool).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_node_id);")
            .execute(&self.pool).await?;

        // Create indexes
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_page ON messages(page_id, timestamp);")
            .execute(&self.pool).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tool_calls_page ON tool_calls(page_id);")
            .execute(&self.pool).await?;

        Ok(())
    }

    pub async fn create_page(&self, page_id: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT OR IGNORE INTO page_meta (page_id, created_at, last_active, turn_count)
             VALUES (?, ?, ?, 0)"
        )
        .bind(page_id)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn append_message(&self, page_id: &str, role: MessageRole, content: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let role_str = match role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
        };

        sqlx::query(
            "INSERT INTO messages (page_id, role, content, timestamp, requires_consolidation)
             VALUES (?, ?, ?, ?, 1)"
        )
        .bind(page_id)
        .bind(role_str)
        .bind(content)
        .bind(now)
        .execute(&self.pool)
        .await?;

        // Update page meta
        sqlx::query(
            "UPDATE page_meta
             SET last_active = ?, turn_count = turn_count + 1
             WHERE page_id = ?"
        )
        .bind(now)
        .bind(page_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn load_recent(&self, page_id: &str, limit: u32) -> Result<Vec<Message>> {
        let rows = sqlx::query_as::<_, Message>(
            "SELECT id, page_id, role, content, token_count, timestamp
             FROM messages
             WHERE page_id = ?
             ORDER BY timestamp ASC
             LIMIT ?"
        )
        .bind(page_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn list_pages(&self) -> Result<Vec<(String, i64, i64, i32)>> {
        let rows = sqlx::query(
            "SELECT page_id, created_at, last_active, turn_count
             FROM page_meta
             ORDER BY last_active DESC"
        )
        .fetch_all(&self.pool)
        .await?;

        let mut list = Vec::new();
        for row in rows {
            let page_id: String = row.get("page_id");
            let created_at: i64 = row.get("created_at");
            let last_active: i64 = row.get("last_active");
            let turn_count: i32 = row.get("turn_count");
            list.push((page_id, created_at, last_active, turn_count));
        }

        Ok(list)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn create_node(&self, id: &str, node_type: &str, label: &str, properties: Option<&str>) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO nodes (node_id, type, label, properties) VALUES (?, ?, ?, ?)"
        )
        .bind(id)
        .bind(node_type)
        .bind(label)
        .bind(properties)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn link_nodes(&self, edge_id: &str, source: &str, target: &str, relation: &str, weight: f64) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO edges (edge_id, source_node_id, target_node_id, relation_type, weight) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(edge_id)
        .bind(source)
        .bind(target)
        .bind(relation)
        .bind(weight)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_nodes_by_type(&self, node_type: &str) -> Result<Value> {
        let rows = sqlx::query(
            "SELECT node_id, type, label, properties FROM nodes WHERE type = ?"
        )
        .bind(node_type)
        .fetch_all(&self.pool)
        .await?;

        let mut nodes_vec = Vec::new();
        for r in rows {
            let id: String = r.get("node_id");
            let t: String = r.get("type");
            let l: String = r.get("label");
            let p: Option<String> = r.get("properties");
            nodes_vec.push(json!({
                "id": id,
                "type": t,
                "label": l,
                "properties": p.and_then(|s| serde_json::from_str::<Value>(&s).ok())
            }));
        }
        Ok(json!(nodes_vec))
    }

    pub async fn delete_node(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM nodes WHERE node_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM messages WHERE page_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM tool_calls WHERE page_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM page_meta WHERE page_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn search_graph(&self, start_node: &str) -> Result<Value> {
        let rows = sqlx::query(
            "WITH RECURSIVE graph_path(node_id, depth) AS (
                SELECT ? AS node_id, 0 AS depth
                UNION
                SELECT e.target_node_id, gp.depth + 1
                FROM graph_path gp
                JOIN edges e ON gp.node_id = e.source_node_id
                WHERE gp.depth < 2
            )
            SELECT n.node_id, n.type, n.label, n.properties
            FROM graph_path gp
            JOIN nodes n ON gp.node_id = n.node_id"
        )
        .bind(start_node)
        .fetch_all(&self.pool)
        .await?;

        let mut nodes_vec = Vec::new();
        for r in rows {
            let id: String = r.get("node_id");
            let t: String = r.get("type");
            let l: String = r.get("label");
            let p: Option<String> = r.get("properties");
            nodes_vec.push(json!({
                "id": id,
                "type": t,
                "label": l,
                "properties": p.and_then(|s| serde_json::from_str::<Value>(&s).ok())
            }));
        }

        let node_ids: Vec<String> = nodes_vec.iter().map(|n| n["id"].as_str().unwrap().to_string()).collect();
        let mut edges_vec = Vec::new();
        if !node_ids.is_empty() {
            let edges_rows = sqlx::query(
                "SELECT edge_id, source_node_id, target_node_id, relation_type, weight FROM edges"
            ).fetch_all(&self.pool).await?;
            for r in edges_rows {
                let s_id: String = r.get("source_node_id");
                let t_id: String = r.get("target_node_id");
                if node_ids.contains(&s_id) && node_ids.contains(&t_id) {
                    let e_id: String = r.get("edge_id");
                    let rel: String = r.get("relation_type");
                    let w: f64 = r.get("weight");
                    edges_vec.push(json!({
                        "edge_id": e_id,
                        "source": s_id,
                        "target": t_id,
                        "relation": rel,
                        "weight": w
                    }));
                }
            }
        }

        Ok(json!({
            "nodes": nodes_vec,
            "edges": edges_vec
        }))
    }

    pub async fn update_page_summary(&self, page_id: &str, summary: &str) -> Result<()> {
        sqlx::query("UPDATE page_meta SET summary = ? WHERE page_id = ?")
            .bind(summary)
            .bind(page_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_page_summary(&self, page_id: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT summary FROM page_meta WHERE page_id = ?")
            .bind(page_id)
            .fetch_optional(&self.pool)
            .await?;
        if let Some(r) = row {
            let summary: Option<String> = r.get("summary");
            Ok(summary)
        } else {
            Ok(None)
        }
    }

    pub async fn truncate_page_messages(&self, page_id: &str, keep_count: u32) -> Result<()> {
        sqlx::query(
            "DELETE FROM messages 
             WHERE page_id = ? 
             AND id NOT IN (
                 SELECT id FROM messages 
                 WHERE page_id = ? 
                 ORDER BY timestamp DESC 
                 LIMIT ?
             )"
        )
        .bind(page_id)
        .bind(page_id)
        .bind(keep_count)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_semantic_memories() {
        let store = SessionStore::new("file:testdb?mode=memory&cache=shared").await.unwrap();

        let id = "test-mem-1";
        let page_id = "test-session";
        let content = "Remember: My favorite game is Minecraft and my cat is named Luna.";
        let importance = 4;
        let tags = vec!["preference".to_string(), "game".to_string()];

        store.insert_memory(id, Some(page_id), content, importance, &tags).await.unwrap();

        let retrieved = store.get_memory(id).await.unwrap().unwrap();
        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.page_id.as_deref(), Some(page_id));
        assert_eq!(retrieved.content, content);
        assert_eq!(retrieved.importance, importance);

        let search_results = store.search_memories_fts("Minecraft").await.unwrap();
        assert_eq!(search_results.len(), 1);
        assert_eq!(search_results[0].id, id);

        let search_results_empty = store.search_memories_fts("Roblox").await.unwrap();
        assert_eq!(search_results_empty.len(), 0);

        let list = store.list_memories().await.unwrap();
        assert_eq!(list.len(), 1);

        store.delete_memory(id).await.unwrap();
        let retrieved_after = store.get_memory(id).await.unwrap();
        assert!(retrieved_after.is_none());

        let list_after = store.list_memories().await.unwrap();
        assert_eq!(list_after.len(), 0);
    }

    #[tokio::test]
    async fn test_hybrid_search_and_context_injection() {
        let id = "test-mem-1";
        let content = "My dog is named Barnaby and he is a brown Labrador.";
        
        let doc1 = hydragent_types::MemoryDocument {
            id: id.to_string(),
            content: content.to_string(),
            timestamp: 1620000000000,
            importance: 4,
            rrf_score: 0.033,
        };
        
        let doc2 = hydragent_types::MemoryDocument {
            id: "test-mem-2".to_string(),
            content: "I prefer working on Rust code.".to_string(),
            timestamp: 1620000001000,
            importance: 5,
            rrf_score: 0.016,
        };

        let base_prompt = "You are a helpful assistant.";
        let injected = crate::build_system_prompt_with_memory(base_prompt, &[doc1.clone(), doc2.clone()], 200);
        assert!(injected.contains("Barnaby"));
        assert!(injected.contains("Rust code"));
        
        // Test strict budget limit (85 tokens) which fits first memory but not the second
        let limited = crate::build_system_prompt_with_memory(base_prompt, &[doc1.clone(), doc2.clone()], 85);
        assert!(limited.contains("Barnaby"));
        assert!(!limited.contains("Rust code"));
    }
}
