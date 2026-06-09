use std::sync::Arc;
use tokio::sync::mpsc;
use serde::Deserialize;
use hydragent_memory::SessionStore;
use hydragent_model::router::ModelRouter;
use hydragent_model::openrouter::ChatMessage;
use tracing::{info, error, warn, debug};

#[derive(Debug, Deserialize)]
struct ExtractionResponse {
    extracted_facts: Vec<ExtractedFact>,
}

#[derive(Debug, Deserialize)]
struct ExtractedFact {
    fact: String,
    category: String,
    importance_1_to_10: u8,
}

#[derive(Debug, Default)]
pub struct DreamStats {
    pub messages_processed: usize,
    pub facts_stored: usize,
    pub facts_skipped: usize,
}

const MIN_IMPORTANCE: u8 = 3;
const BATCH_SIZE: i64 = 100;

pub async fn run_dream_cycle(
    store: Arc<SessionStore>,
    model_router: Arc<ModelRouter>,
) -> anyhow::Result<DreamStats> {
    let mut stats = DreamStats::default();

    // 1. Fetch a batch of unconsolidated messages
    let pool = store.pool();
    let rows = sqlx::query(
        "SELECT id, role, content FROM messages WHERE requires_consolidation = 1 ORDER BY timestamp ASC LIMIT ?"
    )
    .bind(BATCH_SIZE)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        debug!("Dream cycle: no unconsolidated messages, going back to sleep.");
        return Ok(stats);
    }

    info!(batch_size = rows.len(), "Dream cycle: processing message batch");

    // 2. Format as conversation log
    let mut log_lines = Vec::new();
    let mut row_ids = Vec::new();

    use sqlx::Row;
    for row in &rows {
        let id: i64 = row.get("id");
        let role: String = row.get("role");
        let content: String = row.get("content");
        log_lines.push(format!("{}: {}", role, content));
        row_ids.push(id);
        stats.messages_processed += 1;
    }
    let log_text = log_lines.join("\n\n");

    // 3. Build and submit extraction prompt
    let prompt = build_extraction_prompt(&log_text);
    let system_message = ChatMessage {
        role: "user".to_string(),
        content: prompt,
    };

    let (tx, mut rx) = mpsc::channel(100);
    // Spawn task to drain the channel since we are doing non-streaming LLM call for extraction
    tokio::spawn(async move {
        while let Some(_) = rx.recv().await {}
    });

    let (raw_json, _model_used) = match model_router.chat_stream(vec![system_message], tx).await {
        Ok(res) => res,
        Err(e) => {
            error!(error = %e, "Dream cycle: LLM call failed");
            return Ok(stats);
        }
    };

    // 4. Parse JSON
    let extraction = match parse_json_extraction(&raw_json) {
        Some(parsed) => parsed,
        None => {
            warn!(
                raw_output_len = raw_json.len(),
                "Dream cycle: failed to parse extraction JSON — skipping batch to avoid loop"
            );
            // Mark consolidated to prevent forever looping on bad response
            mark_consolidated(pool, &row_ids).await?;
            return Ok(stats);
        }
    };

    // 5. Store extracted facts
    for fact in extraction.extracted_facts {
        if fact.importance_1_to_10 < MIN_IMPORTANCE {
            stats.facts_skipped += 1;
            continue;
        }

        let memory_id = uuid::Uuid::new_v4().to_string();
        let tags = vec![fact.category];

        if let Err(e) = store.insert_memory(
            &memory_id,
            None,
            &fact.fact,
            fact.importance_1_to_10 as i64,
            &tags,
        ).await {
            error!("Dream cycle: failed to store fact: {}", e);
            continue;
        }
        stats.facts_stored += 1;
    }

    // 6. Mark source messages as consolidated
    mark_consolidated(pool, &row_ids).await?;

    info!(?stats, "Dream cycle: batch complete");
    Ok(stats)
}

async fn mark_consolidated(pool: &sqlx::SqlitePool, ids: &[i64]) -> anyhow::Result<()> {
    for id in ids {
        sqlx::query("UPDATE messages SET requires_consolidation = 0 WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

fn build_extraction_prompt(log_text: &str) -> String {
    let template = include_str!("prompts/extraction_prompt.txt");
    template.replace("{CONVERSATION_LOG}", log_text)
}

fn parse_json_extraction(raw: &str) -> Option<ExtractionResponse> {
    let mut cleaned = raw.trim();
    if cleaned.starts_with("```json") {
        cleaned = cleaned.strip_prefix("```json").unwrap_or(cleaned);
    } else if cleaned.starts_with("```") {
        cleaned = cleaned.strip_prefix("```").unwrap_or(cleaned);
    }
    if cleaned.ends_with("```") {
        cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned);
    }
    let cleaned = cleaned.trim();

    let start_idx = cleaned.find('{')?;
    let end_idx = cleaned.rfind('}')?;
    let json_sub = &cleaned[start_idx..=end_idx];

    serde_json::from_str(json_sub).ok()
}
