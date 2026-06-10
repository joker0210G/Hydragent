use crate::session_store::SessionStore;
use crate::models::SemanticMemory;
use anyhow::Result;

impl SessionStore {
    pub async fn insert_memory(
        &self,
        id: &str,
        page_id: Option<&str>,
        content: &str,
        importance: i64,
        tags: &[String],
    ) -> Result<()> {
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
        .bind(importance)
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
        sqlx::query("DELETE FROM semantic_memories WHERE id = ?")
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
