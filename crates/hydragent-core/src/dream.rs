use std::sync::Arc;
use tokio::sync::mpsc;
use serde::Deserialize;
use hydragent_memory::SessionStore;
use hydragent_model::router::ModelRouter;
use hydragent_model::openrouter::ChatMessage;
use tracing::{info, error, warn, debug};

#[derive(Debug, Deserialize)]
struct ExtractionResponse {
    extracted_facts: Option<Vec<ExtractedFact>>,
    style_habits: Option<Vec<String>>,
    behavior_rules: Option<Vec<String>>,
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
    pub style_habits_stored: usize,
    pub behavior_rules_stored: usize,
}

const MIN_IMPORTANCE: u8 = 3;
const BATCH_SIZE: i64 = 100;

pub async fn run_dream_cycle(
    store: Arc<SessionStore>,
    model_router: Arc<ModelRouter>,
) -> anyhow::Result<DreamStats> {
    let mut stats = DreamStats::default();
    let pool = store.pool();

    // 1. Fetch distinct pages that have requires_consolidation messages
    let pages = sqlx::query(
        "SELECT DISTINCT page_id FROM messages WHERE requires_consolidation = 1 LIMIT 5"
    )
    .fetch_all(pool)
    .await?;

    if pages.is_empty() {
        debug!("Dream cycle: no unconsolidated messages, going back to sleep.");
        return Ok(stats);
    }

    use sqlx::Row;
    for page_row in pages {
        let page_id: String = page_row.get("page_id");

        // Fetch a batch of unconsolidated messages for this page
        let rows = sqlx::query(
            "SELECT id, role, content FROM messages WHERE page_id = ? AND requires_consolidation = 1 ORDER BY timestamp ASC LIMIT ?"
        )
        .bind(&page_id)
        .bind(BATCH_SIZE)
        .fetch_all(pool)
        .await?;

        if rows.is_empty() {
            continue;
        }

        info!(page_id = %page_id, batch_size = rows.len(), "Dream cycle: processing message batch for Page");

        // 2. Format as conversation log
        let mut log_lines = Vec::new();
        let mut row_ids = Vec::new();

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
        tokio::spawn(async move {
            while let Some(_) = rx.recv().await {}
        });

        let (raw_json, _model_used) = match model_router.chat_stream(vec![system_message], tx).await {
            Ok(res) => res,
            Err(e) => {
                error!(error = %e, "Dream cycle: LLM call failed");
                continue;
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
                continue;
            }
        };

        // 5. Store extracted facts
        if let Some(facts) = extraction.extracted_facts {
            for fact in facts {
                if fact.importance_1_to_10 < MIN_IMPORTANCE {
                    stats.facts_skipped += 1;
                    continue;
                }

                let memory_id = uuid::Uuid::new_v4().to_string();
                let tags = vec![fact.category];

                if let Err(e) = store.insert_memory(
                    &memory_id,
                    Some(&page_id),
                    &fact.fact,
                    fact.importance_1_to_10 as i64,
                    &tags,
                ).await {
                    error!("Dream cycle: failed to store fact: {}", e);
                    continue;
                }
                stats.facts_stored += 1;
            }
        }

        // 6. Append style habits to USER.md
        if let Some(habits) = extraction.style_habits {
            if !habits.is_empty() {
                if let Err(e) = append_to_markdown_section(
                    "./config/USER.md",
                    "# Style & Communication Habits",
                    &habits,
                    "# User Profile\n- Name: User\n\n# Style & Communication Habits\n"
                ) {
                    error!("Dream cycle: failed to write style habits to USER.md: {}", e);
                } else {
                    stats.style_habits_stored += habits.len();
                }
            }
        }

        // 7. Append behavior rules to SOUL.md
        if let Some(rules) = extraction.behavior_rules {
            if !rules.is_empty() {
                if let Err(e) = append_to_markdown_section(
                    "./config/SOUL.md",
                    "# Behavior Rules",
                    &rules,
                    "# Agent Soul & Personality\n- Name: Hydra\n- Tone: Helpful, intelligent, and adaptive.\n\n# Behavior Rules\n"
                ) {
                    error!("Dream cycle: failed to write rules to SOUL.md: {}", e);
                } else {
                    stats.behavior_rules_stored += rules.len();
                }
            }
        }

        // 8. Mark source messages as consolidated
        mark_consolidated(pool, &row_ids).await?;
    }

    info!(?stats, "Dream cycle: completed all pages");
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

fn append_to_markdown_section(
    file_path: &str,
    section_header: &str,
    items: &[String],
    default_template: &str,
) -> anyhow::Result<()> {
    let path = std::path::Path::new(file_path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut content = std::fs::read_to_string(path).unwrap_or_else(|_| default_template.to_string());

    if !content.contains(section_header) {
        if !content.ends_with('\n') && !content.is_empty() {
            content.push('\n');
        }
        content.push_str("\n");
        content.push_str(section_header);
        content.push_str("\n");
    }

    let mut updated = false;
    for item in items {
        let normalized = item.trim();
        if normalized.is_empty() {
            continue;
        }
        // Check if item is already present (case insensitive and simple substring check)
        let lowered_content = content.to_lowercase();
        let lowered_item = normalized.to_lowercase();
        if !lowered_content.contains(&lowered_item) {
            if !content.ends_with('\n') && !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&format!("- {}\n", normalized));
            updated = true;
        }
    }
    if updated {
        std::fs::write(path, content)?;
    }
    Ok(())
}
