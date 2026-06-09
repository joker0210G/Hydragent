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
        };
        store.init_db().await?;

        Ok(store)
    }

    pub async fn get_embedder(&self) -> Result<&LocalEmbedder> {
        self.embedder.get_or_try_init(|| async {
            let paths = ensure_model_downloaded(&self.data_dir).await?;
            let embedder = LocalEmbedder::new(&paths.model_path, &paths.tokenizer_path)?;
            Ok(embedder)
        }).await
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

        // Add columns if they don't exist
        let _ = sqlx::query("ALTER TABLE messages ADD COLUMN chunk_id TEXT;").execute(&self.pool).await;
        let _ = sqlx::query("ALTER TABLE messages ADD COLUMN requires_consolidation BOOLEAN DEFAULT 1;").execute(&self.pool).await;

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

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS semantic_memories (
                id          TEXT    PRIMARY KEY,
                session_id  TEXT,
                content     TEXT    NOT NULL,
                importance  INTEGER NOT NULL DEFAULT 1,
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
            "INSERT INTO messages (session_id, role, content, timestamp, requires_consolidation)
             VALUES (?, ?, ?, ?, 1)"
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_semantic_memories() {
        let store = SessionStore::new("file:testdb?mode=memory&cache=shared").await.unwrap();

        let id = "test-mem-1";
        let session_id = "test-session";
        let content = "Remember: My favorite game is Minecraft and my cat is named Luna.";
        let importance = 4;
        let tags = vec!["preference".to_string(), "game".to_string()];

        store.insert_memory(id, Some(session_id), content, importance, &tags).await.unwrap();

        let retrieved = store.get_memory(id).await.unwrap().unwrap();
        assert_eq!(retrieved.id, id);
        assert_eq!(retrieved.session_id.as_deref(), Some(session_id));
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
        
        // Test strict budget limit (75 tokens) which fits first memory but not the second
        let limited = crate::build_system_prompt_with_memory(base_prompt, &[doc1.clone(), doc2.clone()], 75);
        assert!(limited.contains("Barnaby"));
        assert!(!limited.contains("Rust code"));
    }
}
