use std::sync::Arc;
use std::collections::HashSet;
use tokio::sync::mpsc;
use serde::Deserialize;
use sqlx::SqlitePool;
use hydragent_memory::SessionStore;
use hydragent_model::router::ModelRouter;
use hydragent_model::openrouter::ChatMessage;
use hydragent_skills::library::SkillLibrary;
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

impl DreamStats {
    /// Sum another `DreamStats` into this one. Used to merge per-page
    /// results from concurrent dream tasks back into the cycle-level
    /// totals without holding a lock around the whole cycle.
    fn merge(&mut self, other: &DreamStats) {
        self.messages_processed += other.messages_processed;
        self.facts_stored += other.facts_stored;
        self.facts_skipped += other.facts_skipped;
        self.style_habits_stored += other.style_habits_stored;
        self.behavior_rules_stored += other.behavior_rules_stored;
    }
}

const MIN_IMPORTANCE: u8 = 3;
const BATCH_SIZE: i64 = 100;

/// Per-page concurrency cap. With up to 5 pages in flight, each page
/// costs one LLM call (~30s). 5 pages sequential = ~150s; 5 pages
/// concurrent ≈ 30s. 5 is also the cap on `SELECT DISTINCT page_id
/// LIMIT 5` below, so this matches the source side.
const MAX_CONCURRENT_PAGES: usize = 5;

/// Dedup threshold for the word-overlap heuristic in
/// `is_duplicate_fact`. If >= this fraction of the *new* fact's
/// significant words are already present in an existing fact, treat
/// the new one as a near-duplicate and skip it. 0.6 = "more than half
/// the same words". Tunable; 0.5–0.7 is the useful range.
const DEDUP_WORD_OVERLAP: f64 = 0.6;

/// Memory-consolidation "dream" cycle. There is one live brain — the
/// `ModelRouter` passed in by the main runtime — used for everything
/// (chat, dreaming, tool routing). The agent doesn't carry two routers
/// anymore; users swap the brain by changing `BRAIN_BASE/KEY/MODEL`
/// in their `.env`.
///
/// Per-page processing is **concurrent** (up to `MAX_CONCURRENT_PAGES`
/// in flight at once) — the prior sequential loop took ~30s × N pages,
/// which blew D2 (test)'s 30s budget on a 5-page backlog. The merge
/// happens at the end of the cycle, so the per-page `DreamStats`
/// fields stay accurate.
pub async fn run_dream_cycle(
    store: Arc<SessionStore>,
    model_router: Arc<ModelRouter>,
    skill_library: Option<Arc<SkillLibrary>>,
) -> anyhow::Result<DreamStats> {
    let mut stats = DreamStats::default();
    let pool = store.pool();
    debug!(
        provider = %model_router.provider_label(),
        "Dream cycle: using live brain"
    );

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
    let page_ids: Vec<String> = pages
        .into_iter()
        .map(|row| row.get::<String, _>("page_id"))
        .collect();

    info!(
        page_count = page_ids.len(),
        max_concurrent = MAX_CONCURRENT_PAGES,
        "Dream cycle: fanning out across pages"
    );

    // 2. Fan out per-page tasks. `SqlitePool` is `Clone` (it's an Arc
    // internally) and `Arc<SessionStore>` / `Arc<ModelRouter>` are
    // already shared, so we can hand each task its own copy.
    let mut tasks = Vec::with_capacity(page_ids.len());
    for page_id in page_ids {
        let store = store.clone();
        let model_router = model_router.clone();
        let page_pool = pool.clone();
        let skill_lib = skill_library.clone();
        tasks.push(tokio::spawn(async move {
            process_page(page_id, store, model_router, page_pool, skill_lib).await
        }));
    }

    // 3. Collect results. All tasks were already spawned in step 2,
    // so they're running concurrently in the background — awaiting
    // them sequentially here just collects the completed results.
    // The SQL `LIMIT 5` upstream caps the number of in-flight LLM
    // calls, which is the actual concurrency bound. We previously
    // had a `chunks(MAX_CONCURRENT_PAGES)` indirection that didn't
    // actually do anything — it iterated `&[JoinHandle]`, which
    // doesn't implement `Future` (only `JoinHandle` itself and
    // `&mut JoinHandle` do), so the loop failed to compile. The
    // fix is to consume the `Vec` and await each handle directly.
    for handle in tasks {
        match handle.await {
            Ok(Ok(page_stats)) => stats.merge(&page_stats),
            Ok(Err(e)) => error!(error = %e, "Dream cycle: page task failed"),
            Err(e) => error!(error = %e, "Dream cycle: page task panicked"),
        }
    }

    info!(?stats, "Dream cycle: completed all pages");
    Ok(stats)
}

/// Process a single page: fetch unconsolidated messages, prompt the
/// LLM for facts/habits/rules, store them, mark messages consolidated.
/// Extracted from `run_dream_cycle` so it can run concurrently per
/// page without holding any cycle-level state.
async fn process_page(
    page_id: String,
    store: Arc<SessionStore>,
    model_router: Arc<ModelRouter>,
    pool: SqlitePool,
    skill_library: Option<Arc<SkillLibrary>>,
) -> anyhow::Result<DreamStats> {
    use sqlx::Row;
    let mut stats = DreamStats::default();

    // Fetch a batch of unconsolidated messages for this page
    let rows = sqlx::query(
        "SELECT id, role, content FROM messages
         WHERE page_id = ? AND requires_consolidation = 1
         ORDER BY timestamp ASC LIMIT ?"
    )
    .bind(&page_id)
    .bind(BATCH_SIZE)
    .fetch_all(&pool)
    .await?;

    if rows.is_empty() {
        return Ok(stats);
    }

    info!(page_id = %page_id, batch_size = rows.len(), "Dream cycle: processing message batch for page");

    // Format as conversation log
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

    // Build and submit extraction prompt. The mpsc channel is required
    // by `chat_stream`'s signature even for non-streaming callers;
    // drain it in a side-task so the stream doesn't block.
    let prompt = build_extraction_prompt(&log_text);
    let system_message = ChatMessage {
        role: "user".to_string(),
        content: prompt,
    };

    let (tx, mut rx) = mpsc::channel(100);
    let drain = tokio::spawn(async move { while let Some(_) = rx.recv().await {} });

    let (raw_json, _model_used) = match model_router.chat_stream(vec![system_message], tx, None).await {
        Ok(res) => res,
        Err(e) => {
            error!(error = %e, page_id = %page_id, "Dream cycle: LLM call failed");
            let _ = drain.await;
            return Ok(stats);
        }
    };
    let _ = drain.await;

    // Parse JSON. If parsing fails, mark consolidated to prevent
    // forever looping on a bad response.
    let extraction = match parse_json_extraction(&raw_json) {
        Some(parsed) => parsed,
        None => {
            warn!(
                page_id = %page_id,
                raw_output_len = raw_json.len(),
                "Dream cycle: failed to parse extraction JSON — skipping batch"
            );
            mark_consolidated(&pool, &row_ids).await?;
            return Ok(stats);
        }
    };

    // Store extracted facts. Each fact is dedup-checked against the
    // current store (C3 fix: dream worker previously re-stored facts
    // that had been deleted or were paraphrases of an existing fact).
    if let Some(facts) = extraction.extracted_facts {
        for fact in facts {
            if fact.importance_1_to_10 < MIN_IMPORTANCE {
                stats.facts_skipped += 1;
                continue;
            }
            if is_duplicate_fact(&store, &fact.fact).await {
                debug!(page_id = %page_id, fact = %fact.fact, "Dream cycle: skipping near-duplicate fact");
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
                error!(page_id = %page_id, error = %e, "Dream cycle: failed to store fact");
                continue;
            }
            stats.facts_stored += 1;
        }
    }

    // Append style habits to USER.md
    if let Some(habits) = extraction.style_habits {
        if !habits.is_empty() {
            if let Err(e) = append_to_markdown_section(
                "./config/USER.md",
                "# Style & Communication Habits",
                &habits,
                "# User Profile\n- Name: User\n\n# Style & Communication Habits\n"
            ) {
                error!(page_id = %page_id, error = %e, "Dream cycle: failed to write style habits to USER.md");
            } else {
                stats.style_habits_stored += habits.len();
            }
        }
    }

    // Append behavior rules to SOUL.md
    if let Some(rules) = extraction.behavior_rules {
        if !rules.is_empty() {
            if let Err(e) = append_to_markdown_section(
                "./config/SOUL.md",
                "# Behavior Rules",
                &rules,
                "# Agent Soul & Personality\n- Name: Hydra\n- Tone: Helpful, intelligent, and adaptive.\n\n# Behavior Rules\n"
            ) {
                error!(page_id = %page_id, error = %e, "Dream cycle: failed to write rules to SOUL.md");
            } else {
                stats.behavior_rules_stored += rules.len();
            }
        }
    }

    // Mark source messages as consolidated
    mark_consolidated(&pool, &row_ids).await?;

    // Phase 7 / Week 27 / Day 6 - skill induction. Now that we've
    // successfully consolidated this page's messages, hand the same
    // trajectory to the SkillExtractor. Failures are logged at WARN
    // and never propagated: a single bad page must not break the
    // dream cycle.
    if let Some(lib) = skill_library {
        let stats_ind = crate::skill_induction::induce_skill_from_page_with_library(
            lib,
            &pool,
            &page_id,
        )
        .await;
        if stats_ind.skills_inserted > 0 {
            info!(
                page_id = %page_id,
                inserted = stats_ind.skills_inserted,
                duplicates = stats_ind.duplicates_skipped,
                rejected = stats_ind.rejected,
                "📚 Dream cycle: skill induction"
            );
        }
    }
    Ok(stats)
}

/// Cheap near-duplicate detector for dream-extracted facts. Uses
/// FTS5 keyword search to find candidate existing facts, then
/// computes a word-overlap ratio. Returns `true` if any candidate
/// has ≥ `DEDUP_WORD_OVERLAP` of the new fact's significant words.
///
/// This is the C3 (test) fix: the dream worker previously
/// re-extracted and re-stored facts that the user had just deleted
/// (because the LLM saw a paraphrased version in the message log).
/// Filtering at insert time prevents the re-creation without
/// requiring a soft-delete column or a full embedding-similarity
/// pass (which would be O(N) per insert).
async fn is_duplicate_fact(store: &SessionStore, fact: &str) -> bool {
    // Tokenize the new fact into "significant" words (length >= 3,
    // lowercased). Punctuation, articles, short words like "is"/"a"
    // don't count toward the overlap.
    let new_words: HashSet<String> = fact
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| w.len() >= 3)
        .collect();
    if new_words.is_empty() {
        return false;
    }

    // FTS5 lookup for candidate existing facts. We only inspect the
    // top few hits — overlap on a long-tail match is unlikely and
    // would just be noise.
    let candidates = match store.search_memories_fts(fact).await {
        Ok(v) => v,
        Err(_) => return false,
    };
    if candidates.is_empty() {
        return false;
    }

    for mem in candidates.iter().take(5) {
        let existing_words: HashSet<String> = mem
            .content
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
            .filter(|w| w.len() >= 3)
            .collect();
        if existing_words.is_empty() {
            continue;
        }
        let intersection = new_words.intersection(&existing_words).count();
        let ratio = intersection as f64 / new_words.len() as f64;
        if ratio >= DEDUP_WORD_OVERLAP {
            return true;
        }
    }
    false
}

async fn mark_consolidated(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<()> {
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
