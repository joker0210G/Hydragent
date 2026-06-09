use serde::{Deserialize, Serialize};

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone)]
pub struct SemanticMemory {
    pub id: String,
    pub session_id: Option<String>,
    pub content: String,
    pub importance: i64,
    pub timestamp: i64,
}

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone)]
pub struct MemoryConsolidationJob {
    pub id: i64,
    pub status: String,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}
