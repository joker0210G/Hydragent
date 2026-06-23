use std::sync::Arc;
use std::collections::HashSet;
use tokio::sync::mpsc;
use serde::Deserialize;
use sqlx::SqlitePool;
use hydragent_memory::SessionStore;
use hydragent_memory::library::{Library, NodeKind};
use hydragent_memory::{BoundedMd, USER_MD_CHAR_LIMIT, SOUL_MD_CHAR_LIMIT};
use hydragent_model::router::ModelRouter;
use hydragent_model::openrouter::ChatMessage;
use hydragent_skills::library::SkillLibrary;
use tracing::{info, error, warn, debug};

#[derive(Debug, Deserialize)]
struct ExtractionResponse {
    summary: Option<String>,
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
    /// Number of Page nodes successfully upserted into the Library
    /// graph this cycle. Distinct from `messages_processed` — a page
    /// only emits one Page node even if it consolidates many messages.
    /// This is the 25% / LLM-side emission counter that pairs with
    /// `LibrarianStats.llm_ops` to compute the design-spec 25/75 split.
    pub pages_emitted: usize,
    /// Number of Pages clustered into Books this cycle. Set by the
    /// cycle-level Graphify pass, not per-page tasks. This is the
    /// 75% / local-ops emission counter that pairs with
    /// `LibrarianStats.local_ops` to compute the design-spec 25/75 split.
    /// Stored as `u64` to match `LibraryStats::pages_clustered` — we
    /// copy values directly between the two structs and a
    /// usize/u64 mismatch would force a lossy cast.
    pub pages_clustered: u64,
    /// Number of Books placed onto Shelves this cycle.
    pub books_organized: u64,
    /// Total local Graphify operations performed this cycle (clustering,
    /// edge writes, label lookups). This is `LibrarianStats.local_ops`
    /// expressed at the cycle level so operators can spot drift.
    pub local_graphify_ops: u64,
    /// Number of LLM compaction passes performed on `config/USER.md`
    /// this cycle. Non-zero means the file was over its
    /// [`USER_MD_CHAR_LIMIT`] budget and was re-synthesized.
    pub compactions_user_md: usize,
    /// Number of LLM compaction passes performed on `config/SOUL.md`
    /// this cycle. Non-zero means the file was over its
    /// [`SOUL_MD_CHAR_LIMIT`] budget and was re-synthesized.
    pub compactions_soul_md: usize,
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
        self.pages_emitted += other.pages_emitted;
        self.compactions_user_md += other.compactions_user_md;
        self.compactions_soul_md += other.compactions_soul_md;
        // The clustering pass runs once at the cycle level, not per-page,
        // so it doesn't participate in `merge()`. Cycle-level
        // `pages_clustered` / `books_organized` / `local_graphify_ops`
        // are set in-place on `stats` by `run_dream_cycle` after the
        // per-page tasks complete.
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

    // ── Step 0: Startup compaction check ─────────────────────────────────
    // Immediately enforce the character budget on both personality files.
    // This handles the first-run case where the files are already over-limit
    // from an era of unbounded appends (Hermes pattern: apply limits now,
    // not just to new appends). Compaction is non-fatal — a failure logs at
    // WARN and the cycle continues.
    {
        let mut user_md_compacted = false;
        let mut soul_md_compacted = false;
        if let Err(e) = startup_compaction_check(
            &model_router,
            &mut user_md_compacted,
            &mut soul_md_compacted,
        ).await {
            warn!(error = %e, "Dream cycle: startup compaction check failed (non-fatal)");
        }
        if user_md_compacted { stats.compactions_user_md += 1; }
        if soul_md_compacted { stats.compactions_soul_md += 1; }
    }

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

    // ── 75% Graphify: cluster pages into Books and Books onto Shelves ──────
    // Per-page tasks have already upserted Page nodes with their tag
    // edges. Now we run a single clustering pass over the whole
    // library. This is the 75% side of the 25/75 split from
    // design spec §2 — pure local work, no LLM call.
    //
    // We do this *after* the per-page fan-out rather than inside each
    // task because the clusterer reads `belongs_to` / `sits_on` edges
    // and the pages would race for the same book ids otherwise. One
    // serial pass at the end keeps the per-page work isolated and
    // the clusterer deterministic.
    let library = Library::new(&store);
    match library.run_clustering_pass().await {
        Ok(lib_stats) => {
            // Surface the 75% / Graphify-side emission totals on the
            // cycle-level `DreamStats` so operators reading a single
            // log line can verify the 25/75 split is in range. The
            // per-page `pages_emitted` is set inside `process_page`
            // and accumulated via `merge()`; the clustering numbers
            // are added here because the clusterer runs once for the
            // whole cycle, not per-page.
            stats.pages_clustered = lib_stats.pages_clustered;
            stats.books_organized = lib_stats.books_organized;
            stats.local_graphify_ops = lib_stats.local_ops();
            info!(
                pages_clustered = lib_stats.pages_clustered,
                books_organized = lib_stats.books_organized,
                local_ops       = lib_stats.local_ops(),
                "Dream cycle: Graphify clustering pass complete"
            );
        }
        Err(e) => {
            // A failed clustering pass is non-fatal — the next dream
            // cycle will retry it. Log at warn so the operator can
            // spot persistent failures.
            warn!(error = %e, "Dream cycle: Graphify clustering pass failed (will retry next cycle)");
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

    // Snapshot the fact categories *before* the facts loop below
    // moves `extraction.extracted_facts`. The page-upsert block at
    // the end of this function needs the same tag list (deduped,
    // lowercased) to write the on-graph `tag` edges the clusterer
    // reads. Computing it once and reusing it keeps the two
    // consumers consistent: a Page always carries exactly the tags
    // derived from its facts.
    let page_tags: Vec<String> = extraction
        .extracted_facts
        .as_ref()
        .map(|facts| {
            let mut seen: HashSet<String> = HashSet::new();
            facts.iter()
                .map(|f| f.category.to_lowercase())
                .filter(|c| seen.insert(c.clone()))
                .collect()
        })
        .unwrap_or_default();

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

    // ── Step 5a: Append style habits → USER.md (bounded) ─────────────────
    if let Some(habits) = extraction.style_habits {
        if !habits.is_empty() {
            let user_bmd = BoundedMd::new("./config/USER.md", USER_MD_CHAR_LIMIT);
            match user_bmd.append_curated(
                &habits,
                "# Style & Communication Habits",
                "# User Profile\n- Name: User\n\n# Style & Communication Habits\n",
            ) {
                Ok(_) => { stats.style_habits_stored += habits.len(); }
                Err(e) => error!(page_id = %page_id, error = %e, "Dream cycle: failed to write style habits to USER.md"),
            }
            // ── Step 5b: LLM compaction if over the budget ───────────────
            if user_bmd.needs_compaction().unwrap_or(false) {
                match compact_md_with_llm(&user_bmd, &model_router, USER_MD_CHAR_LIMIT).await {
                    Ok(()) => {
                        stats.compactions_user_md += 1;
                        info!(page_id = %page_id, "Dream cycle: USER.md compacted via LLM re-synthesis");
                    }
                    Err(e) => warn!(page_id = %page_id, error = %e, "Dream cycle: USER.md compaction failed (non-fatal)"),
                }
            }
        }
    }

    // ── Step 5c: Append behavior rules → SOUL.md (bounded) ───────────────
    if let Some(rules) = extraction.behavior_rules {
        if !rules.is_empty() {
            let soul_bmd = BoundedMd::new("./config/SOUL.md", SOUL_MD_CHAR_LIMIT);
            match soul_bmd.append_curated(
                &rules,
                "# Behavior Rules",
                "# Agent Soul & Personality\n- Name: Hydra\n- Tone: Helpful, intelligent, and adaptive.\n\n# Behavior Rules\n",
            ) {
                Ok(_) => { stats.behavior_rules_stored += rules.len(); }
                Err(e) => error!(page_id = %page_id, error = %e, "Dream cycle: failed to write behavior rules to SOUL.md"),
            }
            // ── Step 5d: LLM compaction if over the budget ───────────────
            if soul_bmd.needs_compaction().unwrap_or(false) {
                match compact_md_with_llm(&soul_bmd, &model_router, SOUL_MD_CHAR_LIMIT).await {
                    Ok(()) => {
                        stats.compactions_soul_md += 1;
                        info!(page_id = %page_id, "Dream cycle: SOUL.md compacted via LLM re-synthesis");
                    }
                    Err(e) => warn!(page_id = %page_id, error = %e, "Dream cycle: SOUL.md compaction failed (non-fatal)"),
                }
            }
        }
    }

    // ── LLM Role (25%): Compress Draft Paper → Page Node ────────────────────
    // The session summary extracted by the LLM becomes a Page node in the
    // Library's `nodes` table, ready for Graphify clustering into Books/Shelves.
    //
    // We use the typed `Library::upsert_node` (not the raw `create_node`)
    // because it (a) records the on-graph `tag` edges the clusterer needs
    // to organise Pages into Books, and (b) uses the typed `NodeKind` enum
    // so this string is checked at compile time, not discovered by a bug
    // report. Tags are derived from the fact categories (deduped) so a
    // page with multiple `preference` facts still only contributes one
    // `preference` tag edge.
    if let Some(ref summary_text) = extraction.summary {
        if !summary_text.trim().is_empty() {
            // Update page_meta summary for compaction context
            if let Err(e) = store.update_page_summary(&page_id, summary_text).await {
                error!(page_id = %page_id, error = %e, "Dream cycle: failed to update page_meta summary");
            }
            // Upsert the Page as a node in the Library graph nodes table
            // so Graphify clustering can discover it as a first-class Page node.
            // `page_tags` was computed above from the same `extracted_facts`
            // list we're about to consume, so it reflects the same deduped
            // lowercased categories the clusterer will read.
            let properties = serde_json::json!({
                "source": "dream",
                "page_id": page_id,
            });
            let library = Library::new(&store);
            match library
                .upsert_node(
                    &page_id,
                    NodeKind::Page,
                    summary_text,
                    &page_tags,
                    Some(&properties),
                )
                .await
            {
                Ok(()) => {
                    info!(
                        page_id = %page_id,
                        summary_len = summary_text.len(),
                        tag_count = page_tags.len(),
                        "Dream cycle: upserted Page node into Library graph"
                    );
                    stats.pages_emitted += 1;
                }
                Err(e) => {
                    error!(page_id = %page_id, error = %e, "Dream cycle: failed to upsert page node in library graph");
                }
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

/// Checks both bounded Markdown files at dream-cycle startup and compacts
/// them via LLM if they are already over their character limits.
///
/// This is the "apply limits now, not just to new appends" step that handles
/// the first-run case where `USER.md` and `SOUL.md` are bloated from an era
/// of unbounded appends. Compaction results are surfaced through the boolean
/// flags so `run_dream_cycle` can update its `DreamStats` counters.
async fn startup_compaction_check(
    model_router: &ModelRouter,
    user_md_compacted: &mut bool,
    soul_md_compacted: &mut bool,
) -> anyhow::Result<()> {
    let user_bmd = BoundedMd::new("./config/USER.md", USER_MD_CHAR_LIMIT);
    if user_bmd.needs_compaction().unwrap_or(false) {
        let current_len = user_bmd.len().unwrap_or(0);
        warn!(
            current_chars = current_len,
            limit = USER_MD_CHAR_LIMIT,
            "Dream cycle startup: USER.md exceeds budget — running LLM compaction"
        );
        compact_md_with_llm(&user_bmd, model_router, USER_MD_CHAR_LIMIT).await?;
        *user_md_compacted = true;
    }

    let soul_bmd = BoundedMd::new("./config/SOUL.md", SOUL_MD_CHAR_LIMIT);
    if soul_bmd.needs_compaction().unwrap_or(false) {
        let current_len = soul_bmd.len().unwrap_or(0);
        warn!(
            current_chars = current_len,
            limit = SOUL_MD_CHAR_LIMIT,
            "Dream cycle startup: SOUL.md exceeds budget — running LLM compaction"
        );
        compact_md_with_llm(&soul_bmd, model_router, SOUL_MD_CHAR_LIMIT).await?;
        *soul_md_compacted = true;
    }

    Ok(())
}

/// Compact an over-limit bounded Markdown file using an LLM re-synthesis call.
///
/// This is the **Hermes true approach**: instead of mechanically truncating
/// old lines, the LLM receives the full over-limit file and rewrites it to
/// fit within `limit` characters — merging near-duplicates, ranking by
/// importance, and preserving the mandatory header block.
///
/// The function follows the same `model_router.chat_stream` pattern used by
/// the extraction call in `process_page`, keeping the LLM interface uniform
/// throughout the dream cycle.
///
/// # Failure modes
/// - LLM call fails → returns `Err`, caller logs at WARN and continues.
/// - LLM output is still over `limit` → returns `Err` (no partial write).
/// - File I/O error → returns `Err`.
async fn compact_md_with_llm(
    bmd: &BoundedMd,
    model_router: &ModelRouter,
    limit: usize,
) -> anyhow::Result<()> {
    let content = bmd.read()?;
    let current_len = content.chars().count();

    if current_len <= limit {
        // Nothing to do — may have been compacted by a concurrent task.
        return Ok(());
    }

    let template = include_str!("prompts/compaction_prompt.txt");
    let prompt = template
        .replace("{LIMIT}", &limit.to_string())
        .replace("{CURRENT_LEN}", &current_len.to_string())
        .replace("{CONTENT}", &content);

    let system_message = ChatMessage {
        role: "user".to_string(),
        content: prompt,
    };

    let (tx, mut rx) = mpsc::channel(100);
    let drain = tokio::spawn(async move { while let Some(_) = rx.recv().await {} });

    let (compacted_text, model_used) = match model_router
        .chat_stream(vec![system_message], tx, None)
        .await
    {
        Ok(res) => res,
        Err(e) => {
            let _ = drain.await;
            return Err(e.into());
        }
    };
    let _ = drain.await;

    // Strip any accidental markdown fences the LLM might have added
    let compacted = compacted_text.trim().trim_start_matches("```markdown")
        .trim_start_matches("```").trim_end_matches("```").trim();

    let compacted_len = compacted.chars().count();
    if compacted_len > limit {
        return Err(anyhow::anyhow!(
            "compact_md_with_llm: LLM output ({} chars) still exceeds limit ({} chars) — not writing",
            compacted_len, limit
        ));
    }

    bmd.write(compacted)?;
    info!(
        path = ?bmd.path(),
        before_chars = current_len,
        after_chars  = compacted_len,
        limit,
        model = %model_used,
        "BoundedMd: LLM re-synthesis compaction complete"
    );
    Ok(())
}

