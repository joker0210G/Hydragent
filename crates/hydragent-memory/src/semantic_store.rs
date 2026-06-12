use crate::session_store::SessionStore;
use crate::models::SemanticMemory;
use anyhow::Result;

impl SessionStore {
    /// Builder-style setter for the soft cap. Mirrors the
    /// `MAX_SEMANTIC_MEMORIES` env var wired in `main.rs`.
    pub fn with_max_memories(&mut self, cap: usize) {
        self.max_memories = cap;
    }

    pub async fn insert_memory(
        &self,
        id: &str,
        page_id: Option<&str>,
        content: &str,
        importance: i64,
        tags: &[String],
    ) -> Result<()> {
        // Defensive clamp: the rest of the system (retrieval ranking,
        // LRU eviction, dream extraction prompt) assumes importance is
        // in 1..=5. Without this, the LLM can hand us 0 or 6+ and we'd
        // happily persist it, silently breaking importance-weighted
        // retrieval. G2 (test) hit this when the dream worker stored
        // importance=6 from a miscalibrated prompt.
        let clamped_importance = importance.clamp(1, 5);
        if clamped_importance != importance {
            tracing::warn!(
                original = importance,
                clamped = clamped_importance,
                "insert_memory: importance out of [1,5] bounds, clamped"
            );
        }

        let embedder = self.get_embedder().await?;
        let vector = embedder.embed_text(content)?;

        let now = chrono::Utc::now().timestamp_millis();

        let mut tx = self.pool().begin().await?;

        sqlx::query(
            "INSERT INTO semantic_memories (id, page_id, content, importance, timestamp)
             VALUES (?, ?, ?, ?, ?)"
        )
        .bind(id)
        .bind(page_id)
        .bind(content)
        .bind(clamped_importance)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        for tag in tags {
            sqlx::query(
                "INSERT INTO memory_tags (memory_id, tag)
                 VALUES (?, ?)"
            )
            .bind(id)
            .bind(tag)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        {
            let mut vs = self.vector_store.lock().unwrap();
            vs.insert(id.to_string(), vector);
            let _ = vs.save_to_disk(&self.vector_store_path);
        }

        // LRU eviction sweep (no-op when the cap is usize::MAX).
        if self.max_memories < usize::MAX {
            self.evict_to_limit(self.max_memories).await?;
        }

        Ok(())
    }

    pub async fn get_memory(&self, id: &str) -> Result<Option<SemanticMemory>> {
        let row = sqlx::query_as::<_, SemanticMemory>(
            "SELECT id, page_id, content, importance, timestamp
             FROM semantic_memories
             WHERE id = ?"
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    pub async fn delete_memory(&self, id: &str) -> Result<()> {
        // Delete from all four stores in lockstep:
        //  1. main table
        //  2. FTS5 index (otherwise we leak ghost rows)
        //  3. tag join table
        //  4. in-memory vector store + persist to disk
        sqlx::query("DELETE FROM semantic_memories WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM semantic_memories_fts WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM memory_tags WHERE memory_id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;

        {
            let mut vs = self.vector_store.lock().unwrap();
            vs.delete(id);
            let _ = vs.save_to_disk(&self.vector_store_path);
        }
        Ok(())
    }

    pub async fn list_memories(&self) -> Result<Vec<SemanticMemory>> {
        let rows = sqlx::query_as::<_, SemanticMemory>(
            "SELECT id, page_id, content, importance, timestamp
             FROM semantic_memories
             ORDER BY timestamp DESC"
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// Total number of rows in `semantic_memories`. Cheap O(1) on SQLite.
    pub async fn count_memories(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM semantic_memories")
            .fetch_one(self.pool())
            .await?;
        use sqlx::Row;
        Ok(row.get::<i64, _>("n"))
    }

    /// LRU-style eviction: when the table has grown past `limit`, delete
    /// the oldest + lowest-importance rows until the count is back at
    /// `limit`. Importance is the primary sort key (1..=5), timestamp ASC
    /// is the tiebreaker — so we drop "old, unimportant" facts first and
    /// keep "new, important" facts.
    ///
    /// Returns the number of rows deleted. No-op when the count is at or
    /// below `limit`. Also sweeps the in-memory `VectorStore` of any
    /// embeddings whose backing row was just removed (the FTS5 index is
    /// kept in sync by the `fts_delete` trigger).
    ///
    /// This is the wiring for PHASE_2.md §5.10 (was previously documented
    /// but not implemented).
    pub async fn evict_to_limit(&self, limit: usize) -> Result<usize> {
        let current = self.count_memories().await? as usize;
        if current <= limit {
            return Ok(0);
        }
        let to_delete = current - limit;

        // Capture the ids we are about to delete so we can sweep them from
        // the in-memory VectorStore afterwards (it lives outside SQLite).
        let doomed: Vec<String> = sqlx::query_as::<_, (String,)>(
            "SELECT id FROM semantic_memories
             ORDER BY importance ASC, timestamp ASC
             LIMIT ?"
        )
        .bind(to_delete as i64)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect();

        // SQLite has no portable `DELETE ... LIMIT`; use a sub-select.
        let placeholders = doomed.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "DELETE FROM semantic_memories WHERE id IN ({})",
            placeholders
        );
        let mut q = sqlx::query(&sql);
        for id in &doomed {
            q = q.bind(id);
        }
        let result = q.execute(self.pool()).await?;

        // Sweep the in-memory VectorStore. FTS5 is auto-synced by triggers.
        {
            let mut vs = self.vector_store.lock().unwrap();
            for id in &doomed {
                vs.delete(id);
            }
            let _ = vs.save_to_disk(&self.vector_store_path);
        }

        Ok(result.rows_affected() as usize)
    }

    pub async fn search_memories_fts(&self, query: &str) -> Result<Vec<SemanticMemory>> {
        let rows = sqlx::query_as::<_, SemanticMemory>(
            "SELECT m.id, m.page_id, m.content, m.importance, m.timestamp
             FROM semantic_memories m
             JOIN semantic_memories_fts f ON m.id = f.id
             WHERE f.content MATCH ?
             ORDER BY rank"
        )
        .bind(query)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    pub async fn search_memories_semantic(&self, query: &str, limit: usize) -> Result<Vec<SemanticMemory>> {
        let embedder = self.get_embedder().await?;
        let query_vector = embedder.embed_text(query)?;

        let nearest = {
            let vs = self.vector_store.lock().unwrap();
            vs.search(&query_vector, limit)
        };

        let mut results = Vec::new();
        for (id, _score) in nearest {
            if let Some(mem) = self.get_memory(&id).await? {
                results.push(mem);
            }
        }
        Ok(results)
    }

    pub async fn clear_all_memories(&self) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM semantic_memories").execute(&mut *tx).await?;
        sqlx::query("DELETE FROM memory_tags").execute(&mut *tx).await?;
        sqlx::query("DELETE FROM semantic_memories_fts").execute(&mut *tx).await?;
        tx.commit().await?;

        {
            let mut vs = self.vector_store.lock().unwrap();
            vs.clear();
            let _ = vs.save_to_disk(&self.vector_store_path);
        }
        Ok(())
    }
}
