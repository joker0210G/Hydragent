//! # The Librarian — Design Spec §2 ("75% Graphify + 25% LLM")
//!
//! The Librarian is the **orchestrator** of the ingestion loop. It
//! splits the work between two collaborators:
//!
//! * **LLM (25% weight)** — summarises the Draft Paper into a Page
//!   and extracts personality / style / behavior rules.
//! * **Library / Graphify (75% weight)** — performs all the local
//!   graph operations (page ingestion, tag clustering, shelf
//!   organisation).
//!
//! The two are deliberately decoupled so the LLM-side and the
//! Graphify-side can evolve independently. The [`LibrarianStats`]
//! counters let downstream code report the actual 25/75 split.
//!
//! ## Cost tracking
//!
//! [`LibrarianStats::llm_ops`] counts every LLM call the Librarian
//! makes. [`crate::library::LibraryStats::local_ops`] counts every
//! local Graphify operation. The split ratio at the end of a cycle is
//! therefore:
//!
//! ```text
//! llm_weight    = llm_ops / (llm_ops + local_ops)
//! graphify_wt   = local_ops / (llm_ops + local_ops)
//! ```
//!
//! The spec targets `graphify_wt ≈ 0.75`. If the actual ratio
//! drifts above 0.30 of LLM weight the caller can investigate
//! whether the LLM prompt template is asking for work that could be
//! done locally.
//!
//! ## LLM-side interface
//!
//! The Librarian does not bind to a particular LLM SDK. Instead, it
//! takes a [`LlmSummariser`] trait object so the orchestrator can
//! plug in the `ModelRouter`, a test double, or a stub that returns
//! canned output.

use crate::library::{Library, LibraryStats, NodeKind};
use anyhow::{Context, Result};
use async_trait::async_trait;
use hydragent_types::MemoryDocument;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The 25%-weight LLM collaborator.
///
/// Implementations should:
///
/// 1. Receive a chunk of Draft Paper (conversation log).
/// 2. Return a [`LlmSummary`] containing the page summary, the
///    extracted facts, the style habits, and the behavior rules.
/// 3. Increment their own internal counters so the cost ratio can
///    be reported by the caller.
#[async_trait]
pub trait LlmSummariser: Send + Sync {
    /// Summarise a Draft Paper. Returning `Ok(None)` signals the
    /// summariser decided the conversation was not worth a Page
    /// (e.g. empty log, all small-talk).
    async fn summarise(&self, draft_paper: &str) -> Result<Option<LlmSummary>>;

    /// Total LLM calls made by this summariser since process start.
    /// Used to compute the actual 25/75 split.
    fn call_count(&self) -> u64;
}

/// The 25%-weight LLM's structured output for one Page.
///
/// Mirrors the JSON shape in
/// `crates/hydragent-core/src/prompts/extraction_prompt.txt`, minus
/// the `extracted_facts` (those go through the Graphify side and
/// end up in the `semantic_memories` table, not as graph nodes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSummary {
    /// ≤ 200-word standalone summary. Becomes the Page node label
    /// in the Library.
    pub summary: String,
    /// Capability / domain tags inferred from the conversation.
    /// Used by the clusterer to organise Pages into Books.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Long-term factual memories. These go into
    /// `semantic_memories` (NOT the graph), so the LLM-side is
    /// responsible for emitting them.
    #[serde(default)]
    pub facts: Vec<LlmFact>,
    /// Style / communication habits (→ USER.md).
    #[serde(default)]
    pub style_habits: Vec<String>,
    /// Behavior rules (→ SOUL.md).
    #[serde(default)]
    pub behavior_rules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmFact {
    pub fact: String,
    pub category: String,
    pub importance_1_to_10: u8,
}

/// LLM-side counters for the 25% weight. Pairs with
/// [`LibraryStats::local_ops`] for the 75% weight.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibrarianStats {
    /// Number of LLM calls made during the cycle (or all-time, if
    /// the summariser's lifetime is the process lifetime).
    pub llm_ops: u64,
    /// Number of Pages produced by the LLM summariser.
    pub pages_emitted: u64,
    /// Number of long-term facts handed to `semantic_memories`.
    pub facts_emitted: u64,
    /// Number of style habits handed to USER.md.
    pub style_habits_emitted: u64,
    /// Number of behavior rules handed to SOUL.md.
    pub behavior_rules_emitted: u64,
    /// Local Graphify operations performed in the same cycle. This
    /// is a snapshot — we copy the counters out of [`LibraryStats`]
    /// for unified reporting.
    pub local_ops: u64,
    /// Cycles run. Lets callers compute averages.
    pub cycles: u64,
}

impl LibrarianStats {
    /// Approximate fraction of work done by the LLM. Computed as
    /// `llm_ops / (llm_ops + local_ops)`. Returns 0.0 if no work
    /// was done.
    pub fn llm_weight(&self) -> f64 {
        let total = self.llm_ops as f64 + self.local_ops as f64;
        if total <= 0.0 { 0.0 } else { self.llm_ops as f64 / total }
    }

    /// Approximate fraction done by Graphify (local). Target ≈ 0.75.
    /// Returns 0.0 when no work was performed (matches `llm_weight`).
    pub fn graphify_weight(&self) -> f64 {
        let total = self.llm_ops as f64 + self.local_ops as f64;
        if total <= 0.0 { 0.0 } else { self.local_ops as f64 / total }
    }
}

/// Result of a single ingestion cycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestionResult {
    pub page_id: Option<String>,
    pub librarian: LibrarianStats,
    pub library: LibraryStats,
}

impl IngestionResult {
    pub fn merge(&mut self, other: &IngestionResult) {
        self.librarian.llm_ops                 += other.librarian.llm_ops;
        self.librarian.pages_emitted           += other.librarian.pages_emitted;
        self.librarian.facts_emitted           += other.librarian.facts_emitted;
        self.librarian.style_habits_emitted    += other.librarian.style_habits_emitted;
        self.librarian.behavior_rules_emitted  += other.librarian.behavior_rules_emitted;
        self.librarian.local_ops               += other.librarian.local_ops;
        self.librarian.cycles                  += other.librarian.cycles;
        self.library.merge(&other.library);
        if self.page_id.is_none() {
            self.page_id = other.page_id.clone();
        }
    }
}

/// Optional callback that the caller can plug in to persist long-
/// term facts emitted by the LLM into the existing
/// `semantic_memories` table. Kept as a trait so the Librarian
/// crate doesn't take a hard dependency on `SessionStore::insert_*`.
#[async_trait]
pub trait FactSink: Send + Sync {
    async fn store_fact(
        &self,
        page_id: Option<&str>,
        fact: &str,
        importance: i64,
        tags: &[String],
    ) -> Result<()>;

    async fn store_user_insight(
        &self,
        page_id: &str,
        insight: &str,
    ) -> Result<()>;
}

/// Optional callback for personality / style outputs that get
/// appended to `USER.md` and `SOUL.md`. Same decoupling rationale
/// as [`FactSink`].
#[async_trait]
pub trait PersonalitySink: Send + Sync {
    async fn append_style_habit(&self, habit: &str) -> Result<()>;
    async fn append_behavior_rule(&self, rule: &str) -> Result<()>;
}

/// No-op implementations for tests and dry-runs.
pub struct NoopFactSink;
#[async_trait]
impl FactSink for NoopFactSink {
    async fn store_fact(
        &self,
        _page_id: Option<&str>,
        _fact: &str,
        _importance: i64,
        _tags: &[String],
    ) -> Result<()> {
        Ok(())
    }
    async fn store_user_insight(
        &self,
        _page_id: &str,
        _insight: &str,
    ) -> Result<()> {
        Ok(())
    }
}

pub struct NoopPersonalitySink;
#[async_trait]
impl PersonalitySink for NoopPersonalitySink {
    async fn append_style_habit(&self, _habit: &str) -> Result<()> { Ok(()) }
    async fn append_behavior_rule(&self, _rule: &str) -> Result<()> { Ok(()) }
}

/// The Librarian — the orchestrator that ties LLM and Graphify
/// together.
pub struct Librarian {
    /// Bound at construction. Cheap to clone (`Arc`).
    llm: Arc<dyn LlmSummariser>,
    /// Optional sinks. Default to no-op so callers can opt into
    /// only the parts they care about (e.g. a unit test that just
    /// wants to verify clustering).
    fact_sink: Arc<dyn FactSink>,
    personality_sink: Arc<dyn PersonalitySink>,
}

impl Librarian {
    /// Construct a Librarian with default no-op sinks.
    pub fn new(llm: Arc<dyn LlmSummariser>) -> Self {
        Self {
            llm,
            fact_sink: Arc::new(NoopFactSink),
            personality_sink: Arc::new(NoopPersonalitySink),
        }
    }

    /// Inject the fact sink (long-term memories → semantic_memories).
    pub fn with_fact_sink(mut self, sink: Arc<dyn FactSink>) -> Self {
        self.fact_sink = sink;
        self
    }

    /// Inject the personality sink (style habits / behavior rules
    /// → USER.md / SOUL.md).
    pub fn with_personality_sink(mut self, sink: Arc<dyn PersonalitySink>) -> Self {
        self.personality_sink = sink;
        self
    }

    /// Run one ingestion cycle on a single Draft Paper.
    ///
    /// Sequence (design spec §2):
    ///
    /// ```text
    /// [Draft Paper] ──► [LLM 25%] ──► LlmSummary
    ///                                  │
    ///                                  ▼
    ///                          [Library 75%]
    ///                                  │
    ///                                  ├─► ingest_page(summary, tags)
    ///                                  ├─► cluster_pages_into_books()
    ///                                  └─► organize_books_onto_shelves()
    /// ```
    ///
    /// Long-term facts emitted by the LLM are forwarded to the
    /// [`FactSink`]. Style habits go to [`PersonalitySink`].
    pub async fn ingest(
        &self,
        page_id: &str,
        draft_paper: &str,
        library: &Library<'_>,
    ) -> Result<IngestionResult> {
        let mut result = IngestionResult::default();

        // ── 25% LLM ─────────────────────────────────────────────────
        // Short-circuit on empty paper: don't burn an LLM call just
        // to learn the conversation was a no-op.
        if draft_paper.is_empty() {
            return Ok(result);
        }
        let llm_before = self.llm.call_count();
        let summary = self.llm.summarise(draft_paper).await?;
        let llm_after = self.llm.call_count();
        result.librarian.llm_ops = llm_after.saturating_sub(llm_before);

        let Some(summary) = summary else {
            // The LLM decided the conversation was not worth a Page.
            return Ok(result);
        };
        result.librarian.pages_emitted += 1;

        // ── 75% Graphify ────────────────────────────────────────────
        // (1) Upsert the Page node with the LLM-emitted tags. The
        //     upsert also writes the per-tag edges used by the
        //     clusterer.
        library.upsert_node(
            page_id,
            NodeKind::Page,
            &summary.summary,
            &summary.tags,
            None,
        ).await.context("ingest: upsert page node")?;
        let mut lib_stats = LibraryStats::default();
        lib_stats.pages_ingested += 1;
        lib_stats.edges_linked    += summary.tags.len() as u64;

        // (2) Cluster the unlinked pages into Books.
        let books_created = library.cluster_unlinked_pages().await?;
        lib_stats.books_created   += books_created;
        lib_stats.pages_clustered += books_created;

        // (3) Organise Books onto Shelves.
        let shelves_created = library.organize_books_onto_shelves().await?;
        lib_stats.shelves_created += shelves_created;
        lib_stats.books_organized += shelves_created;

        // ── Long-term facts (LLM-side writes, delegated to sinks) ─
        for fact in &summary.facts {
            self.fact_sink.store_fact(
                Some(page_id),
                &fact.fact,
                fact.importance_1_to_10 as i64,
                std::slice::from_ref(&fact.category),
            ).await?;
            result.librarian.facts_emitted += 1;
        }
        for habit in &summary.style_habits {
            self.personality_sink.append_style_habit(habit).await?;
            result.librarian.style_habits_emitted += 1;
        }
        for rule in &summary.behavior_rules {
            self.personality_sink.append_behavior_rule(rule).await?;
            result.librarian.behavior_rules_emitted += 1;
        }

        result.page_id = Some(page_id.to_string());
        result.librarian.local_ops = lib_stats.local_ops();
        result.library = lib_stats;
        Ok(result)
    }

    /// Run a Graphify-only pass (no LLM call). Useful when the
    /// caller has already produced pages externally (e.g. via the
    /// existing `dream.rs` flow) and just wants to re-cluster /
    /// re-organise the graph. Cost: 0% LLM, 100% Graphify.
    pub async fn run_graphify_pass(
        &self,
        library: &Library<'_>,
    ) -> Result<IngestionResult> {
        let mut result = IngestionResult::default();
        let lib_stats = library.run_clustering_pass().await?;
        result.library = lib_stats.clone();
        result.librarian.local_ops = lib_stats.local_ops();
        Ok(result)
    }
}

/// Convert a [`LlmSummary`] into the existing
/// [`hydragent_types::MemoryDocument`] shape so it can be fed
/// straight into [`crate::hybrid_search`] / the context injector.
/// This keeps the new types and the rest of the agent runtime
/// decoupled: the existing `dream.rs` flow already builds
/// `MemoryDocument`s, so the Librarian just needs a one-line
/// adapter to do the same.
pub fn summary_to_memory_doc(page_id: &str, summary: &LlmSummary) -> MemoryDocument {
    MemoryDocument {
        id: format!("page:{}", page_id),
        content: format!("[Page] {}", summary.summary),
        timestamp: chrono::Utc::now().timestamp_millis(),
        importance: 5,
        rrf_score: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Stub LLM summariser that returns a canned response and
    /// increments a counter. Used by the unit tests below.
    struct StubLlm {
        counter: AtomicU64,
        canned: Option<LlmSummary>,
    }

    #[async_trait]
    impl LlmSummariser for StubLlm {
        async fn summarise(&self, _draft_paper: &str) -> Result<Option<LlmSummary>> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(self.canned.clone())
        }
        fn call_count(&self) -> u64 { self.counter.load(Ordering::SeqCst) }
    }

    fn stub_with(summary: LlmSummary) -> Arc<StubLlm> {
        Arc::new(StubLlm {
            counter: AtomicU64::new(0),
            canned: Some(summary),
        })
    }

    async fn fresh_store() -> crate::SessionStore {
        // `cache=shared` is required so every connection in the
        // sqlx pool sees the schema created on the first
        // connection. With `cache=private` each pool connection
        // would see a fresh empty DB and all queries would fail
        // with "no such table".
        let url = format!(
            "file:librarian_test_{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4()
        );
        crate::SessionStore::new(&url).await.unwrap()
    }

    #[test]
    fn librarian_stats_weight_ratio() {
        let mut s = LibrarianStats::default();
        s.llm_ops = 25;
        s.local_ops = 75;
        assert!((s.llm_weight() - 0.25).abs() < 1e-9);
        assert!((s.graphify_weight() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn librarian_stats_zero_work() {
        let s = LibrarianStats::default();
        assert_eq!(s.llm_weight(), 0.0);
        assert_eq!(s.graphify_weight(), 0.0);
    }

    #[tokio::test]
    async fn ingest_creates_page_books_shelves_and_reports_split() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        let summary = LlmSummary {
            summary: "Session focused on Rust async patterns".to_string(),
            tags: vec!["rust".into(), "async".into()],
            facts: vec![LlmFact {
                fact: "User prefers Tokio".into(),
                category: "preference".into(),
                importance_1_to_10: 7,
            }],
            style_habits: vec!["Uses parenthetical meta-thoughts".into()],
            behavior_rules: vec!["Always use Markdown code blocks".into()],
        };
        let llm = stub_with(summary);
        let librarian = Librarian::new(llm.clone());

        let result = librarian
            .ingest("page-1", "some draft paper content", &lib)
            .await
            .unwrap();

        assert_eq!(result.page_id.as_deref(), Some("page-1"));
        assert_eq!(result.librarian.pages_emitted, 1);
        assert_eq!(result.librarian.facts_emitted, 1);
        assert_eq!(result.librarian.style_habits_emitted, 1);
        assert_eq!(result.librarian.behavior_rules_emitted, 1);
        assert_eq!(result.librarian.llm_ops, 1);
        assert!(result.librarian.local_ops > 0);
        // 25/75-ish split: 1 LLM call vs N local ops
        assert!(result.librarian.graphify_weight() > 0.0);

        // The Page was actually upserted.
        let found = lib.find_by_label(NodeKind::Page, "Session focused on Rust async patterns")
            .await.unwrap();
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn empty_paper_skips_llm_call() {
        let store = fresh_store().await;
        let lib = Library::new(&store);
        let llm = stub_with(LlmSummary {
            summary: "should not appear".into(),
            tags: vec![],
            facts: vec![],
            style_habits: vec![],
            behavior_rules: vec![],
        });
        let librarian = Librarian::new(llm.clone());

        let result = librarian.ingest("page-x", "", &lib).await.unwrap();
        assert_eq!(result.librarian.llm_ops, 0);
        assert!(result.page_id.is_none());
    }

    #[tokio::test]
    async fn run_graphify_pass_uses_zero_llm_calls() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        // Pre-seed a page so the clusterer has something to do.
        lib.upsert_node("p1", NodeKind::Page, "rust async",
            &["rust".into(), "async".into()], None).await.unwrap();

        let llm = stub_with(LlmSummary {
            summary: "unused".into(), tags: vec![], facts: vec![],
            style_habits: vec![], behavior_rules: vec![],
        });
        let librarian = Librarian::new(llm.clone());

        let result = librarian.run_graphify_pass(&lib).await.unwrap();
        assert_eq!(result.librarian.llm_ops, 0);
        assert_eq!(llm.call_count(), 0, "graphify pass must not invoke LLM");
        assert!(result.librarian.local_ops > 0);
    }
}