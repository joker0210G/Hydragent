use std::sync::atomic::{AtomicUsize, Ordering};
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

use std::sync::Mutex;
use std::path::PathBuf;
use std::sync::OnceLock;

struct CompactionCacheEntry {
    path: PathBuf,
    len: u64,
    mtime: u64,
}

static COMPACTION_CACHE: OnceLock<Mutex<Vec<CompactionCacheEntry>>> = OnceLock::new();

fn should_check_compaction(path: &std::path::Path) -> bool {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return true,
    };
    let len = metadata.len();
    let mtime = metadata.modified()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)))
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cache = COMPACTION_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut cache_guard = cache.lock().unwrap();
    if let Some(entry) = cache_guard.iter_mut().find(|e| e.path == path) {
        if entry.len == len && entry.mtime == mtime {
            return false;
        }
        entry.len = len;
        entry.mtime = mtime;
    } else {
        cache_guard.push(CompactionCacheEntry {
            path: path.to_path_buf(),
            len,
            mtime,
        });
    }
    true
}

#[derive(Debug, Deserialize)]
struct ExtractionResponse {
    summary: Option<String>,
    suggested_books: Option<Vec<String>>,
    suggested_shelves: Option<Vec<String>>,
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
    /// Number of extraction JSON parse failures across all retries.
    pub extraction_failures: usize,
    /// Total retry attempts used across all pages.
    pub extraction_retries: usize,
    /// Facts skipped because they were near-duplicates of existing memories.
    pub dedup_hits: usize,
    /// Pages skipped because they contained no meaningful data (trivial/greeting).
    pub pages_skipped_trivial: usize,
    /// Facts that were inferred rather than verbatim in the conversation log.
    pub facts_inferred: usize,
    /// Facts that appeared verbatim (or near-verbatim) in the conversation log.
    pub facts_verbatim: usize,
    /// Page tasks that failed (panic or error) and produced no stats.
    pub pages_failed: usize,
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
        self.extraction_failures += other.extraction_failures;
        self.extraction_retries += other.extraction_retries;
        self.dedup_hits += other.dedup_hits;
        self.pages_skipped_trivial += other.pages_skipped_trivial;
        self.facts_inferred += other.facts_inferred;
        self.facts_verbatim += other.facts_verbatim;
        self.pages_failed += other.pages_failed;
        // The clustering pass runs once at the cycle level, not per-page,
        // so it doesn't participate in `merge()`. Cycle-level
        // `pages_clustered` / `books_organized` / `local_graphify_ops`
        // are set in-place on `stats` by `run_dream_cycle` after the
        // per-page tasks complete.
    }
}

const MIN_IMPORTANCE: u8 = 3;
const BATCH_SIZE: i64 = 100;

/// Dedup threshold for the word-overlap heuristic in
/// `is_duplicate_fact`. If >= this fraction of the *new* fact's
/// significant words are already present in an existing fact, treat
/// the new one as a near-duplicate and skip it. 0.6 = "more than half
/// the same words". Tunable; 0.5–0.7 is the useful range.
const DEDUP_WORD_OVERLAP: f64 = 0.6;

/// Circuit breaker: how many consecutive dream cycles can fail before
/// we skip cycles to avoid burning API quota.
const MAX_CONSECUTIVE_FAILURES: usize = 3;

static CONSECUTIVE_FAILURES: AtomicUsize = AtomicUsize::new(0);

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

    // ── Circuit breaker check ──────────────────────────────────────────────
    // If too many consecutive cycles failed, skip this one to avoid burning
    // API quota on a dead or rate-limited brain.
    let failures = CONSECUTIVE_FAILURES.load(Ordering::SeqCst);
    if failures >= MAX_CONSECUTIVE_FAILURES {
        warn!(
            failures,
            max = MAX_CONSECUTIVE_FAILURES,
            "Dream cycle: circuit breaker open — skipping cycle"
        );
        return Ok(stats);
    }

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
        "Dream cycle: processing pages sequentially via queue"
    );

    // 2. Process pages sequentially in a queue (FIFO)
    let mut success_count = 0usize;
    let mut fail_count = 0usize;
    for page_id in page_ids {
        match process_page(
            page_id,
            store.clone(),
            model_router.clone(),
            pool.clone(),
            skill_library.clone(),
        )
        .await
        {
            Ok(page_stats) => {
                stats.merge(&page_stats);
                success_count += 1;
            }
            Err(e) => {
                error!(error = %e, "Dream cycle: page task failed");
                stats.pages_failed += 1;
                fail_count += 1;
            }
        }
    }

    // Update circuit breaker: reset on any success, increment on all-failure.
    if success_count > 0 {
        CONSECUTIVE_FAILURES.store(0, Ordering::SeqCst);
    } else if fail_count > 0 {
        let new_count = CONSECUTIVE_FAILURES.fetch_add(1, Ordering::SeqCst) + 1;
        warn!(
            new_count,
            max = MAX_CONSECUTIVE_FAILURES,
            "Dream cycle: all page tasks failed, circuit breaker warming"
        );
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

    // Trigger the Python graph generator to rebuild the D3 graph at ~/.hydragent/data/graph.html
    let python_bin = if cfg!(target_os = "windows") {
        let local_venv = std::path::Path::new(".venv").join("Scripts").join("python.exe");
        if local_venv.exists() {
            local_venv.to_string_lossy().to_string()
        } else {
            "python".to_string()
        }
    } else {
        let local_venv = std::path::Path::new(".venv").join("bin").join("python");
        if local_venv.exists() {
            local_venv.to_string_lossy().to_string()
        } else {
            "python3".to_string()
        }
    };

    let mut cmd = tokio::process::Command::new(python_bin);
    cmd.args(&["-m", "graphing.main"]);
    match cmd.spawn() {
        Ok(mut child) => {
            tokio::spawn(async move {
                match child.wait().await {
                    Ok(status) => {
                        if !status.success() {
                            warn!("Graph generation script exited with non-zero status: {:?}", status);
                        } else {
                            info!("Graph generation script completed successfully.");
                        }
                    }
                    Err(e) => {
                        error!("Failed to wait on graph generation child process: {}", e);
                    }
                }
            });
        }
        Err(e) => {
            error!("Failed to spawn graph generation process: {}", e);
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
    let mut log_text = log_lines.join("\n\n");
    let word_count = log_text.split_whitespace().count();
    let estimated_tokens = (word_count as f64 * 1.3) as usize;
    if estimated_tokens > 8192 {
        let keep_lines = log_lines.len();
        let start_keep = (keep_lines as f64 * 0.2) as usize;
        let end_keep = (keep_lines as f64 * 0.4) as usize;
        if start_keep + end_keep < keep_lines {
            let mut truncated = log_lines[..start_keep].to_vec();
            truncated.push("... [CONVERSATION LOG TRUNCATED FROM MIDDLE TO SAVE TOKEN BUDGET] ...".to_string());
            truncated.extend_from_slice(&log_lines[keep_lines - end_keep..]);
            log_text = truncated.join("\n\n");
            info!(page_id = %page_id, before_lines = keep_lines, after_lines = truncated.len(), "Dream cycle: truncated long conversation log from the middle");
        }
    }

    // Extract with retry: up to 3 attempts with stricter prompts on failure.
    let extraction = match extract_with_retry(&model_router, &log_text, &page_id, &mut stats).await {
        Some(ex) => ex,
        None => {
            warn!(page_id = %page_id, "Dream cycle: extraction failed after all retries — deferring consolidation for a future retry");
            stats.pages_failed += 1;
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
                .map(|f| normalize_category(&f.category))
                .filter(|c| seen.insert(c.clone()))
                .collect()
        })
        .unwrap_or_default();

    // Store extracted facts. Each fact is dedup-checked against the
    // current store (C3 fix: dream worker previously re-stored facts
    // that had been deleted or were paraphrases of an existing fact).
    if let Some(facts) = extraction.extracted_facts {
        for fact in facts {
            let category = normalize_category(&fact.category);

            if fact.importance_1_to_10 < MIN_IMPORTANCE {
                stats.facts_skipped += 1;
                continue;
            }

            // Grounding check: is the fact supported by the conversation log?
            let grounded = fact_is_grounded(&fact.fact, &log_text);
            if !grounded && fact.importance_1_to_10 < 6 {
                debug!(page_id = %page_id, fact = %fact.fact, "Dream cycle: skipping ungrounded low-importance fact");
                stats.facts_skipped += 1;
                continue;
            }
            if grounded {
                stats.facts_verbatim += 1;
            } else {
                stats.facts_inferred += 1;
            }

            if is_duplicate_fact(&store, &fact.fact).await {
                debug!(page_id = %page_id, fact = %fact.fact, "Dream cycle: skipping near-duplicate fact");
                stats.facts_skipped += 1;
                stats.dedup_hits += 1;
                continue;
            }
            let memory_id = uuid::Uuid::new_v4().to_string();
            let tags = vec![category];
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
            let user_bmd = BoundedMd::new(crate::paths::config_dir().join("USER.md"), USER_MD_CHAR_LIMIT);
            match user_bmd.append_curated(
                &habits,
                "# Style & Communication Habits",
                "# User Profile\n- Name: User\n- Role: Software Engineer & Technical Operator\n- Preferred Tone: Professional, direct, and technically rigorous\n- Language & Locale: English (Universal)\n- Key Constraints: Absolute precision, strict formatting compliance, zero fluff\n\n# Style & Communication Habits\n",
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
            let soul_bmd = BoundedMd::new(crate::paths::config_dir().join("SOUL.md"), SOUL_MD_CHAR_LIMIT);
            match soul_bmd.append_curated(
                &rules,
                "# Behavior Rules",
                "# Agent Soul & Personality\n- Name: Hydra\n- Role: Advanced Agentic AI Coding Assistant & Technical Sparring Partner\n- Tone: Professional, precise, adaptive, and concise\n- Core Guidelines: Prioritize execution, maintain high technical depth, respect security boundaries, avoid placeholders\n- Language Capability: Global (English primary)\n\n# Behavior Rules\n",
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
    let has_meaningful_data = {
        let has_facts = stats.facts_stored > 0;
        let has_habits = stats.style_habits_stored > 0;
        let has_rules = stats.behavior_rules_stored > 0;
        
        let is_trivial_summary = if let Some(ref s) = extraction.summary {
            let s_lower = s.to_lowercase();
            s_lower.contains("trivial") || s_lower.contains("greeting") || s_lower.contains("no substantive") || s.trim().is_empty()
        } else {
            true
        };
        
        (has_facts || has_habits || has_rules) && !is_trivial_summary
    };

    if has_meaningful_data {
        if let Some(ref summary_text) = extraction.summary {
            if !summary_text.trim().is_empty() {
                // Update page_meta summary for compaction context
                if let Err(e) = store.update_page_summary(&page_id, summary_text).await {
                    error!(page_id = %page_id, error = %e, "Dream cycle: failed to update page_meta summary");
                }
                // Upsert the Page as a node in the Library graph nodes table
                let properties = serde_json::json!({
                    "source": "dream",
                    "page_id": page_id,
                    "suggested_books": extraction.suggested_books,
                    "suggested_shelves": extraction.suggested_shelves,
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
    } else {
        stats.pages_skipped_trivial += 1;
        info!(
            page_id = %page_id,
            "Dream cycle: Page has no meaningful data (no new facts, habits, or rules) — skipping graph node insertion to prevent clutter"
        );
    }

    // Mark source messages as consolidated
    mark_consolidated(&pool, &row_ids).await?;

    // Phase 7 / Week 27 / Day 6 - skill induction. Now that we've
    // successfully consolidated this page's messages, hand the same
    // trajectory to the SkillExtractor. Failures are logged at WARN
    // and never propagated: a single bad page must not break the
    // dream cycle.
    if let Some(lib) = skill_library {
        let stats_ind = crate::skill_induction::induce_skill_from_page_with_library_and_router(
            lib,
            &pool,
            &page_id,
            Some(model_router),
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

    // Fast word-overlap check: if any candidate has overlap > 0.3,
    // we run the full embedding similarity check on it.
    let mut has_potential_dup = false;
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
        if ratio >= 0.3 {
            has_potential_dup = true;
            break;
        }
    }

    if !has_potential_dup {
        return false;
    }

    // Run embedding similarity check against HNSW vector index
    if let Ok(embedder) = store.get_embedder().await {
        if let Ok(query_vector) = embedder.embed_text(fact) {
            let nearest = {
                let vs = store.vector_store().lock().unwrap();
                vs.search(&query_vector, 5)
            };
            for (_id, similarity) in nearest {
                if similarity >= 0.85 {
                    return true;
                }
            }
        }
    }

    // If embedding check didn't find a match, fall back to high word overlap
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
    if ids.is_empty() {
        return Ok(());
    }
    if ids.len() == 1 {
        sqlx::query("UPDATE messages SET requires_consolidation = 0 WHERE id = ?")
            .bind(&ids[0])
            .execute(pool)
            .await?;
        return Ok(());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!("UPDATE messages SET requires_consolidation = 0 WHERE id IN ({})", placeholders);
    let mut q = sqlx::query(&query);
    for id in ids {
        q = q.bind(id);
    }
    q.execute(pool).await?;
    Ok(())
}

fn build_extraction_prompt(log_text: &str) -> (String, String) {
    let system = include_str!("prompts/extraction_system.txt");
    let user_template = include_str!("prompts/extraction_user.txt");
    let user = user_template.replace("{CONVERSATION_LOG}", log_text);
    (system.to_string(), user)
}

fn normalize_category(cat: &str) -> String {
    let lower = cat.to_lowercase().trim().to_string();
    match lower.as_str() {
        "work" | "project" | "task" | "job" => "project_state".to_string(),
        "general" | "misc" | "other" => "personal".to_string(),
        "tech" | "coding" | "dev" | "development" | "software" => "technical".to_string(),
        "pref" | "like" | "dislike" | "favorite" => "preference".to_string(),
        "personal" | "technical" | "preference" | "project_state" => lower,
        _ => {
            warn!(category = %cat, "Dream cycle: unknown fact category, defaulting to 'personal'");
            "personal".to_string()
        }
    }
}

fn fact_is_grounded(fact: &str, log_text: &str) -> bool {
    let fact_norm = fact.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != ' ', "");
    let log_norm = log_text.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != ' ', "");
    if log_norm.contains(&fact_norm) {
        return true;
    }
    let fact_lower = fact.to_lowercase();
    let fact_words: HashSet<&str> = fact_lower
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.len() >= 4)
        .collect();
    if fact_words.is_empty() {
        return true; // Too short to validate, assume grounded
    }
    let log_lower = log_text.to_lowercase();
    let log_words: HashSet<&str> = log_lower
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.len() >= 4)
        .collect();
    let matched = fact_words.intersection(&log_words).count();
    matched * 10 >= fact_words.len() * 6
}

async fn extract_with_retry(
    model_router: &ModelRouter,
    log_text: &str,
    page_id: &str,
    stats: &mut DreamStats,
) -> Option<ExtractionResponse> {
    let (system_prompt, user_prompt) = build_extraction_prompt(log_text);

    let attempts = [
        user_prompt.clone(),
        format!("{}\n\nCRITICAL: Respond with ONLY a valid JSON object. No markdown fences, no explanation, no commentary.", user_prompt),
        format!("{}\n\nOUTPUT ONLY JSON. NOTHING ELSE. NO ```. NO TEXT BEFORE OR AFTER THE JSON.", user_prompt),
    ];

    for (attempt, user) in attempts.iter().enumerate() {
        if attempt > 0 {
            stats.extraction_retries += 1;
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }

        let messages = vec![
            ChatMessage { role: "system".to_string(), content: system_prompt.clone() },
            ChatMessage { role: "user".to_string(), content: user.clone() },
        ];

        let (tx, mut rx) = mpsc::channel(100);
        let drain = tokio::spawn(async move { while let Some(_) = rx.recv().await {} });

        match model_router.chat_stream(messages, tx, None).await {
            Ok((raw_json, _)) => {
                let _ = drain.await;
                if let Some(extraction) = parse_json_extraction(&raw_json) {
                    return Some(extraction);
                }
                stats.extraction_failures += 1;
                warn!(page_id = %page_id, attempt = attempt + 1, "Dream cycle: JSON parse failed");
            }
            Err(e) => {
                let _ = drain.await;
                error!(page_id = %page_id, attempt = attempt + 1, error = %e, "Dream cycle: LLM call failed");
                if attempt < attempts.len() - 1 {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    None
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
    let user_path = crate::paths::config_dir().join("USER.md");
    if should_check_compaction(&user_path) {
        let user_bmd = BoundedMd::new(user_path, USER_MD_CHAR_LIMIT);
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
    }

    let soul_path = crate::paths::config_dir().join("SOUL.md");
    if should_check_compaction(&soul_path) {
        let soul_bmd = BoundedMd::new(soul_path, SOUL_MD_CHAR_LIMIT);
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
    if compacted_len == 0 {
        anyhow::bail!("compact_md_with_llm: LLM returned empty content — not writing");
    }

    let original_header = content.lines().next().unwrap_or("").trim();
    if !original_header.is_empty() && !compacted.contains(original_header) {
        anyhow::bail!(
            "compact_md_with_llm: compacted content is missing the original header {:?} — not writing",
            original_header
        );
    }

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

