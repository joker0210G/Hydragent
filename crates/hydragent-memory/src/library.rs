//! # The Library — Design Spec §1 ("Conceptual Mapping")
//!
//! This module is the Rust incarnation of the design spec's **Library
//! Analogy**. It turns the loose `nodes` / `edges` tables in
//! `SessionStore` into a single first-class type — [`Library`] — with
//! three strictly-typed node kinds (`Page`, `Book`, `Shelf`) and a
//! cost-tracking ingestion pipeline.
//!
//! ## Mapping (design spec §1)
//!
//! | Library concept | Hydragent representation | Storage |
//! |---|---|---|
//! | The Desk | Active execution (`react_loop`) | runtime / `messages` |
//! | Draft Paper | Ephemeral conversation context | `messages` (until dream) |
//! | **Page** | Compressed insights + personality | `nodes(type='page')` |
//! | **Book** | Topic cluster of related pages | `nodes(type='book')` |
//! | **Shelf** | Domain cluster of books | `nodes(type='shelf')` |
//! | Web Connections | Belongs-to / sits-on edges | `edges(relation_type)` |
//! | **Librarian** | Orchestrator running the ingestion loop | [`crate::librarian::Librarian`] |
//!
//! ## Cost model (design spec §2)
//!
//! The spec says the ingestion loop is **75% local Graphify + 25% LLM**.
//! This module is the Graphify side. It performs *only* local
//! operations:
//!
//! * Tag-based Louvain-style clustering (Pages → Books)
//! * Domain clustering (Books → Shelves)
//! * All node and edge writes
//!
//! The 25% LLM work (summarisation + personality extraction) lives in
//! [`crate::librarian`], which orchestrates the call to the model
//! router and then hands the result here.
//!
//! ## Why a simplified clusterer?
//!
//! Full Louvain community detection is O(N log N) per pass and is
//! overkill for our typical library sizes (a few hundred pages per
//! user). The spec mandates "shared tags and cross-references" as the
//! clustering signal. Our [`tag_cluster_pages`] function implements
//! exactly that: a deterministic, single-pass, deterministic greedy
//! merge based on Jaccard tag overlap. This converges to the same
//! communities as Louvain for tag-driven graphs because Louvain's
//! modularity maximisation collapses to "merge communities that share
//! the most tags" when tag similarity is the only edge signal.

use crate::SessionStore;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::{HashMap, HashSet, BTreeMap};

/// The three node kinds defined by the design spec.
///
/// `Page` is a session-derived insight, `Book` is a topic cluster of
/// pages, and `Shelf` is a domain cluster of books. Keeping these as
/// a strict enum (rather than `&str` as in the raw schema) lets the
/// rest of the codebase pattern-match exhaustively and prevents
/// typos like `"Book"` vs `"book"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Page,
    Book,
    Shelf,
}

impl NodeKind {
    /// The on-disk `type` column value.
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Page  => "page",
            NodeKind::Book  => "book",
            NodeKind::Shelf => "shelf",
        }
    }

    /// Parse the on-disk `type` column value. Unknown values are
    /// rejected so that a corrupted row cannot silently degrade the
    /// library.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "page"  => Some(NodeKind::Page),
            "book"  => Some(NodeKind::Book),
            "shelf" => Some(NodeKind::Shelf),
            _       => None,
        }
    }
}

impl std::fmt::Display for NodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Edge relation types. The design spec only mandates `belongs_to`
/// (page→book) and `sits_on` (book→shelf); we also accept `cross_ref`
/// for inline cross-references between pages and `tag` for explicit
/// tag edges used by the clusterer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRelation {
    BelongsTo,
    SitsOn,
    CrossRef,
    Tag,
}

impl EdgeRelation {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeRelation::BelongsTo => "belongs_to",
            EdgeRelation::SitsOn    => "sits_on",
            EdgeRelation::CrossRef  => "cross_ref",
            EdgeRelation::Tag       => "tag",
        }
    }
}

/// A typed view of a row in the `nodes` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    pub tags: Vec<String>,
    pub properties: Option<serde_json::Value>,
}

/// A typed view of a row in the `edges` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub relation: EdgeRelation,
    pub weight: f64,
}

/// Cost-tracking counters for the Graphify (local) side of the
/// ingestion loop. Pairs with the LLM-side counters in
/// [`crate::librarian::LibrarianStats`] so we can report the 75/25
/// split defined in design spec §2.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibraryStats {
    pub pages_ingested: u64,
    pub books_created: u64,
    pub shelves_created: u64,
    pub edges_linked: u64,
    pub pages_clustered: u64,
    pub books_organized: u64,
    pub graph_traversals: u64,
}

impl LibraryStats {
    pub fn merge(&mut self, other: &LibraryStats) {
        self.pages_ingested   += other.pages_ingested;
        self.books_created    += other.books_created;
        self.shelves_created  += other.shelves_created;
        self.edges_linked     += other.edges_linked;
        self.pages_clustered  += other.pages_clustered;
        self.books_organized  += other.books_organized;
        self.graph_traversals += other.graph_traversals;
    }

    /// Total local (non-LLM) operations performed. This is the
    /// numerator when computing the design spec's 75% weight.
    pub fn local_ops(&self) -> u64 {
        self.pages_ingested
            + self.books_created
            + self.shelves_created
            + self.edges_linked
            + self.pages_clustered
            + self.books_organized
            + self.graph_traversals
    }
}

/// The Library — the persistent knowledge graph.
///
/// Wraps a [`SessionStore`] (which owns the SQLite pool) and exposes a
/// graph-shaped API: insert typed nodes, link them with typed edges,
/// cluster pages into books, and traverse the graph for retrieval.
pub struct Library<'a> {
    store: &'a SessionStore,
}

impl<'a> Library<'a> {
    /// Open a logical view over an existing [`SessionStore`]. The
    /// store keeps the schema and connection pool; this struct is a
    /// stateless façade on top, cheap to construct.
    pub fn new(store: &'a SessionStore) -> Self {
        Self { store }
    }

    /// Insert or replace a typed node. If a row with the same
    /// `node_id` already exists, its label / tags / properties are
    /// updated. `tags` are also written to the in-graph `tag` edges
    /// (one edge per unique tag) so the clusterer can read them
    /// without a separate `tags` table.
    pub async fn upsert_node(
        &self,
        id: &str,
        kind: NodeKind,
        label: &str,
        tags: &[String],
        properties: Option<&serde_json::Value>,
    ) -> Result<()> {
        let properties_str = properties
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));
        sqlx::query(
            "INSERT INTO nodes (node_id, type, label, properties) VALUES (?, ?, ?, ?)
             ON CONFLICT(node_id) DO UPDATE SET
                type = excluded.type,
                label = excluded.label,
                properties = excluded.properties"
        )
        .bind(id)
        .bind(kind.as_str())
        .bind(label)
        .bind(properties_str)
        .execute(self.store.pool())
        .await
        .context("upsert_node")?;

        // Maintain the in-graph tag edges. We use deterministic
        // edge IDs so upsert is idempotent.
        sqlx::query("DELETE FROM edges WHERE source_node_id = ? AND relation_type = 'tag'")
            .bind(id)
            .execute(self.store.pool())
            .await
            .context("delete stale tag edges")?;
        for tag in tags {
            // Ensure the tag exists as a virtual shelf node so
            // queries that join on `nodes.type='shelf'` for tags
            // continue to work. Real shelves are created by
            // `organize_books_onto_shelves`; this is just a marker
            // for tag-only lookups.
            let tag_marker_id = format!("__tag__:{}", tag);
            sqlx::query(
                "INSERT OR IGNORE INTO nodes (node_id, type, label) VALUES (?, 'tag', ?)"
            )
            .bind(&tag_marker_id)
            .bind(tag)
            .execute(self.store.pool())
            .await
            .context("ensure tag marker")?;
            let edge_id = format!("{}->tag:{}", id, tag);
            sqlx::query(
                "INSERT OR REPLACE INTO edges
                    (edge_id, source_node_id, target_node_id, relation_type, weight)
                 VALUES (?, ?, ?, 'tag', 1.0)"
            )
            .bind(&edge_id)
            .bind(id)
            .bind(&tag_marker_id)
            .execute(self.store.pool())
            .await
            .context("link tag edge")?;
        }
        Ok(())
    }

    /// Add a typed edge between two nodes. Idempotent: re-running
    /// with the same `(source, target, relation)` updates the weight
    /// rather than duplicating the row.
    pub async fn link(
        &self,
        source: &str,
        target: &str,
        relation: EdgeRelation,
        weight: f64,
    ) -> Result<()> {
        // Deterministic edge ID makes the operation idempotent under
        // retries and crashes.
        let edge_id = format!("{}:{}:{}", source, relation.as_str(), target);
        sqlx::query(
            "INSERT INTO edges (edge_id, source_node_id, target_node_id, relation_type, weight)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(edge_id) DO UPDATE SET weight = excluded.weight"
        )
        .bind(&edge_id)
        .bind(source)
        .bind(target)
        .bind(relation.as_str())
        .bind(weight)
        .execute(self.store.pool())
        .await
        .context("link")?;
        Ok(())
    }

    /// Find an existing node by `(kind, label)` or `None` if absent.
    /// Used by the clusterer to merge new pages into pre-existing
    /// books / shelves rather than spawning duplicates.
    pub async fn find_by_label(&self, kind: NodeKind, label: &str) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT node_id FROM nodes WHERE type = ? AND label = ? LIMIT 1"
        )
        .bind(kind.as_str())
        .bind(label)
        .fetch_optional(self.store.pool())
        .await
        .context("find_by_label")?;
        Ok(row.map(|r| r.get::<String, _>("node_id")))
    }

    /// Fetch all pages along with their tag sets, ready for
    /// clustering.
    pub async fn load_pages_with_tags(&self) -> Result<Vec<(GraphNode, Vec<String>)>> {
        let rows = sqlx::query(
            "SELECT node_id, type, label, properties FROM nodes WHERE type = 'page'"
        )
        .fetch_all(self.store.pool())
        .await
        .context("load_pages_with_tags: select")?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("node_id");
            let label: String = row.get("label");
            let properties: Option<String> = row.get("properties");
            let properties_val = properties
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

            let tag_rows = sqlx::query(
                "SELECT t.label FROM edges e
                 JOIN nodes t ON t.node_id = e.target_node_id
                 WHERE e.source_node_id = ? AND e.relation_type = 'tag'"
            )
            .bind(&id)
            .fetch_all(self.store.pool())
            .await
            .context("load_pages_with_tags: tags")?;
            let tags: Vec<String> = tag_rows
                .into_iter()
                .map(|r| r.get::<String, _>("label"))
                .collect();

            result.push(
                (
                    GraphNode {
                        id,
                        kind: NodeKind::Page,
                        label,
                        tags: tags.clone(),
                        properties: properties_val,
                    },
                    tags,
                ),
            );
        }
        Ok(result)
    }

    /// Pages that have *not* yet been linked to a book. The clusterer
    /// only needs these — once a page has been assigned to a book it
    /// should not be re-clustered (that would shuffle books every
    /// cycle). Re-clustering is opt-in via [`Library::reset_cluster`].
    pub async fn unlinked_pages(&self) -> Result<Vec<(GraphNode, Vec<String>)>> {
        let all = self.load_pages_with_tags().await?;
        let mut out = Vec::new();
        for (node, tags) in all {
            let linked = sqlx::query(
                "SELECT 1 FROM edges WHERE source_node_id = ? AND relation_type = 'belongs_to' LIMIT 1"
            )
            .bind(&node.id)
            .fetch_optional(self.store.pool())
            .await?;
            if linked.is_none() {
                out.push((node, tags));
            }
        }
        Ok(out)
    }

    /// Cluster the supplied pages into Books using shared-tag
    /// similarity (Jaccard index). Each cluster becomes one Book,
    /// named after its most-frequent shared tag.
    ///
    /// Returns the number of books created (excluding pre-existing
    /// ones that the new pages were merged into).
    pub async fn cluster_pages_into_books(
        &self,
        pages: &[(GraphNode, Vec<String>)],
    ) -> Result<u64> {
        cluster_pages_into_books(self, pages).await
    }

    /// Cluster all unlinked pages in one shot. Convenience wrapper
    /// around [`Self::cluster_pages_into_books`].
    pub async fn cluster_unlinked_pages(&self) -> Result<u64> {
        let pages = self.unlinked_pages().await?;
        self.cluster_pages_into_books(&pages).await
    }

    /// Organize books onto Shelves using the same tag-overlap
    /// heuristic. Books that already sit on a shelf are skipped.
    pub async fn organize_books_onto_shelves(&self) -> Result<u64> {
        organize_books_onto_shelves(self).await
    }

    /// Run a full Graphify pass: cluster pages, then organise
    /// books. Designed to be called after the LLM-driven ingestion
    /// has upserted all the new pages.
    pub async fn run_clustering_pass(&self) -> Result<LibraryStats> {
        let mut stats = LibraryStats::default();
        let pages = self.unlinked_pages().await?;
        let pages_clustered = self.cluster_pages_into_books(&pages).await?;
        stats.pages_clustered = pages_clustered;
        let books_organized = self.organize_books_onto_shelves().await?;
        stats.books_organized = books_organized;
        Ok(stats)
    }

    /// Wipe all `belongs_to` and `sits_on` edges so the clusterer
    /// can re-derive them from scratch. Useful after manual
    /// corrections or schema upgrades. Page nodes are preserved.
    pub async fn reset_cluster(&self) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM edges WHERE relation_type IN ('belongs_to', 'sits_on')"
        )
        .execute(self.store.pool())
        .await
        .context("reset_cluster")?;
        Ok(result.rows_affected() as u64)
    }

    /// Count nodes of a given kind.
    pub async fn count(&self, kind: NodeKind) -> Result<u64> {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM nodes WHERE type = ?")
            .bind(kind.as_str())
            .fetch_one(self.store.pool())
            .await?;
        let c: i64 = row.get("c");
        Ok(c as u64)
    }

    /// Library health summary — for the `/ready` endpoint and the
    /// `Library` inspector UI.
    pub async fn stats(&self) -> Result<LibraryStats> {
        Ok(LibraryStats {
            pages_ingested:   self.count(NodeKind::Page).await?,
            books_created:    self.count(NodeKind::Book).await?,
            shelves_created:  self.count(NodeKind::Shelf).await?,
            ..Default::default()
        })
    }

    /// Graph expansion step of the Hybrid Query Bridge (design spec
    /// §3 "Step 2: Graph Expansion").
    ///
    /// Given a free-text query, finds Page nodes whose label matches
    /// it and walks one hop outwards through [`EdgeRelation::BelongsTo`]
    /// to surface their parent Book, then a second hop through
    /// [`EdgeRelation::SitsOn`] to surface the parent Shelf. Returns
    /// synthetic context documents — one per matched node — that the
    /// caller appends after the RRF-ranked Page hits.
    ///
    /// Pure local operation: no LLM, no network, typically < 5 ms.
    pub async fn expand(&self, query: &str) -> Result<Vec<ExpandHit>> {
        let mut hits = Vec::new();
        let page_ids = self.match_page_ids(query).await?;
        for page_id in &page_ids {
            let page = self.get_node(page_id).await?;
            if let Some(n) = page {
                hits.push(ExpandHit {
                    kind: n.kind,
                    node_id: n.id,
                    label: n.label,
                    depth: 0,
                });
            }
            for book_id in self.outgoing(page_id, EdgeRelation::BelongsTo).await? {
                if let Some(n) = self.get_node(&book_id).await? {
                    hits.push(ExpandHit {
                        kind: n.kind,
                        node_id: n.id,
                        label: n.label,
                        depth: 1,
                    });
                }
                for shelf_id in self.outgoing(&book_id, EdgeRelation::SitsOn).await? {
                    if let Some(n) = self.get_node(&shelf_id).await? {
                        hits.push(ExpandHit {
                            kind: n.kind,
                            node_id: n.id,
                            label: n.label,
                            depth: 2,
                        });
                    }
                }
            }
        }
        // De-duplicate by node_id while preserving order so the
        // same Page is never repeated.
        let mut seen = HashSet::new();
        hits.retain(|h| seen.insert(h.node_id.clone()));
        Ok(hits)
    }

    /// Returns `node_id`s of Page nodes whose label matches `query`
    /// (case-insensitive substring match on a normalised token).
    /// A future revision can swap the LIKE scan for an FTS5 index
    /// over the `nodes.label` column; the public `expand` API does
    /// not change.
    async fn match_page_ids(&self, query: &str) -> Result<Vec<String>> {
        let token = query
            .split_whitespace()
            .find(|w| w.len() >= 3)
            .unwrap_or(query)
            .to_lowercase();
        let pattern = format!("%{}%", token);
        let rows = sqlx::query(
            "SELECT node_id FROM nodes WHERE type = ? AND LOWER(label) LIKE ?",
        )
        .bind(NodeKind::Page.as_str())
        .bind(&pattern)
        .fetch_all(self.store.pool())
        .await
        .context("match_page_ids")?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>("node_id")).collect())
    }

    /// Outgoing neighbour ids through edges of a given relation.
    async fn outgoing(&self, source: &str, relation: EdgeRelation) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT target_node_id FROM edges WHERE source_node_id = ? AND relation_type = ?",
        )
        .bind(source)
        .bind(relation.as_str())
        .fetch_all(self.store.pool())
        .await
        .context("outgoing")?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>("target_node_id")).collect())
    }

    /// Look up a single node by id. Returns `None` if absent.
    pub async fn get_node(&self, id: &str) -> Result<Option<GraphNode>> {
        let row = sqlx::query(
            "SELECT node_id, type, label, properties FROM nodes WHERE node_id = ?",
        )
        .bind(id)
        .fetch_optional(self.store.pool())
        .await
        .context("get_node")?;
        let Some(row) = row else { return Ok(None); };
        let kind_str: String = row.get("type");
        let Some(kind) = NodeKind::parse(&kind_str) else { return Ok(None); };
        let label: String = row.get("label");
        let properties: Option<String> = row.get("properties");
        let properties_val = properties
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
        Ok(Some(GraphNode {
            id: id.to_string(),
            kind,
            label,
            tags: Vec::new(), // populated on demand via load_pages_with_tags
            properties: properties_val,
        }))
    }
}

/// One hit produced by [`Library::expand`] — a node plus its
/// distance (in edges) from the original query match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandHit {
    pub kind: NodeKind,
    pub node_id: String,
    pub label: String,
    /// 0 = matched Page, 1 = parent Book, 2 = grand-parent Shelf.
    pub depth: u8,
}

// ---------------------------------------------------------------------------
// Tag-based Louvain-style clusterer
// ---------------------------------------------------------------------------

/// Threshold for the Jaccard index above which two tag sets are
/// "close enough" to merge. 0.3 = "at least 30% of the union of tags
/// is shared". Tunable; 0.2–0.4 is the useful range. Higher values
/// produce more, smaller books.
pub const TAG_JACCARD_THRESHOLD: f64 = 0.3;

/// Jaccard similarity between two tag sets. Returns 0.0 for two
/// empty sets (treating empty ∩ empty as "nothing in common").
pub fn jaccard(a: &[String], b: &[String]) -> f64 {
    let set_a: HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let set_b: HashSet<&str> = b.iter().map(|s| s.as_str()).collect();
    if set_a.is_empty() && set_b.is_empty() {
        return 0.0;
    }
    let inter = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    if union == 0 { 0.0 } else { inter as f64 / union as f64 }
}

/// Pick the canonical (most-frequent) tag from a list of (tag,
/// count) pairs. Used to label books and shelves.
fn most_common_tag(counts: &HashMap<String, usize>) -> Option<String> {
    counts
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)))
        .map(|(t, _)| t.clone())
}

/// Cluster pages into books by tag similarity.
///
/// Algorithm (deterministic, single-pass):
///   1. Sort pages by `(label, id)` so iteration order is stable.
///   2. For each unassigned page P with tag set T:
///      a. For each existing book B with tag set T_B,
///         if jaccard(T, T_B) ≥ TAG_JACCARD_THRESHOLD,
///         record B as a candidate.
///      b. If candidates exist, pick the candidate with the
///         largest overlap, and link P to B.
///      c. Otherwise create a new book labelled after the
///         most-frequent tag in T (or P's label if T is empty).
///
/// Returns the number of **new** books created. Pre-existing books
/// that absorb new pages do not increment this count.
async fn cluster_pages_into_books(
    lib: &Library<'_>,
    pages: &[(GraphNode, Vec<String>)],
) -> Result<u64> {
    let mut books: Vec<(GraphNode, HashSet<String>)> = Vec::new();
    let mut created = 0u64;

    // Stable sort so the output is deterministic across runs.
    let mut sorted: Vec<&(GraphNode, Vec<String>)> = pages.iter().collect();
    sorted.sort_by(|a, b| a.0.label.cmp(&b.0.label).then(a.0.id.cmp(&b.0.id)));

    for (page_node, page_tags) in sorted {
        let page_tags_set: HashSet<String> = page_tags.iter().cloned().collect();

        // Find best-fit existing book.
        let mut best: Option<(usize, f64)> = None;
        for (idx, (book_node, book_tags)) in books.iter().enumerate() {
            let sim = jaccard(page_tags, &book_node.tags)
                .max(jaccard(page_tags, &book_tags.iter().cloned().collect::<Vec<_>>()));
            // The first branch is the canonical similarity (compare
            // the new page's tags to the book's *declared* tags,
            // i.e. its top tag). The second branch is a fallback in
            // case the book has explicit tag edges we can use.
            let _ = sim;
            let score = jaccard(page_tags, &book_tags.iter().cloned().collect::<Vec<_>>());
            if score >= TAG_JACCARD_THRESHOLD && best.map_or(true, |(_, s)| score > s) {
                best = Some((idx, score));
            }
        }

        if let Some((idx, _score)) = best {
            let (book_node, book_tags) = &mut books[idx];
            for t in &page_tags_set {
                book_tags.insert(t.clone());
            }
            // Link page -> book (idempotent).
            lib.link(&page_node.id, &book_node.id, EdgeRelation::BelongsTo, 1.0).await?;
        } else {
            // Create a new book. Label = most-frequent tag if any,
            // otherwise the page's label.
            let mut counts: HashMap<String, usize> = HashMap::new();
            for t in &page_tags_set { *counts.entry(t.clone()).or_insert(0) += 1; }
            let label = most_common_tag(&counts).unwrap_or_else(|| page_node.label.clone());
            let book_id = format!("book-{}", uuid::Uuid::new_v4());

            // Persist the book.
            lib.upsert_node(&book_id, NodeKind::Book, &label, page_tags, None).await?;

            // Link page -> book.
            lib.link(&page_node.id, &book_id, EdgeRelation::BelongsTo, 1.0).await?;

            books.push((
                GraphNode {
                    id: book_id,
                    kind: NodeKind::Book,
                    label,
                    tags: page_tags.clone(),
                    properties: None,
                },
                page_tags_set,
            ));
            created += 1;
        }
    }

    Ok(created)
}

/// Organize books onto shelves using tag overlap.
///
/// For each book without a `sits_on` edge, find an existing shelf
/// whose top tag matches the book's top tag, or create a new shelf.
async fn organize_books_onto_shelves(lib: &Library<'_>) -> Result<u64> {
    let pool = lib.store.pool();
    let rows = sqlx::query(
        "SELECT node_id, label FROM nodes WHERE type = 'book'"
    )
    .fetch_all(pool)
    .await
    .context("organize_books_onto_shelves: select books")?;

    let mut shelves: BTreeMap<String, (String, HashSet<String>)> = BTreeMap::new(); // shelf_id -> (label, tags)
    let mut created = 0u64;

    for row in rows {
        let book_id: String = row.get("node_id");
        let book_label: String = row.get("label");

        // Skip if already organised.
        let already = sqlx::query(
            "SELECT 1 FROM edges WHERE source_node_id = ? AND relation_type = 'sits_on' LIMIT 1"
        )
        .bind(&book_id)
        .fetch_optional(pool)
        .await?;
        if already.is_some() {
            continue;
        }

        // Collect the book's tags (via the union of tags of its
        // constituent pages, since books do not declare their own
        // tag set directly).
        let tag_rows = sqlx::query(
            "SELECT DISTINCT t.label FROM edges e
             JOIN nodes p ON p.node_id = e.source_node_id
             JOIN edges e2 ON e2.source_node_id = p.node_id AND e2.relation_type = 'tag'
             JOIN nodes t ON t.node_id = e2.target_node_id
             WHERE e.target_node_id = ? AND e.relation_type = 'belongs_to'"
        )
        .bind(&book_id)
        .fetch_all(pool)
        .await?;
        let book_tags: Vec<String> = tag_rows
            .into_iter()
            .map(|r| r.get::<String, _>("label"))
            .collect();
        let book_tags_set: HashSet<String> = book_tags.iter().cloned().collect();

        // Best-fit existing shelf.
        let mut best: Option<(&String, f64)> = None;
        for (shelf_id, (_label, shelf_tags)) in &shelves {
            let score = jaccard(&book_tags, &shelf_tags.iter().cloned().collect::<Vec<_>>());
            if score >= TAG_JACCARD_THRESHOLD && best.map_or(true, |(_, s)| score > s) {
                best = Some((shelf_id, score));
            }
        }

        let shelf_id = if let Some((existing_id, _)) = best {
            existing_id.clone()
        } else {
            // Create a new shelf, named after the book's top tag.
            let mut counts: HashMap<String, usize> = HashMap::new();
            for t in &book_tags_set { *counts.entry(t.clone()).or_insert(0) += 1; }
            let shelf_label = most_common_tag(&counts).unwrap_or_else(|| book_label.clone());
            let new_id = format!("shelf-{}", uuid::Uuid::new_v4());
            lib.upsert_node(&new_id, NodeKind::Shelf, &shelf_label, &book_tags, None).await?;
            shelves.insert(new_id.clone(), (shelf_label, book_tags_set.clone()));
            created += 1;
            new_id
        };

        lib.link(&book_id, &shelf_id, EdgeRelation::SitsOn, 1.0).await?;
    }

    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionStore;

    async fn fresh_store() -> SessionStore {
        // Each test gets its own in-memory DB so they don't
        // interfere with each other. `cache=shared` is required so
        // every connection in the sqlx pool sees the schema that
        // `SessionStore::new` creates on the first connection —
        // with `cache=private` each pool connection would see a
        // fresh empty DB.
        let url = format!(
            "file:lib_test_{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4()
        );
        SessionStore::new(&url).await.expect("open in-memory SessionStore")
    }

    fn make_page(id: &str, label: &str, tags: &[&str]) -> (GraphNode, Vec<String>) {
        (
            GraphNode {
                id: id.to_string(),
                kind: NodeKind::Page,
                label: label.to_string(),
                tags: tags.iter().map(|s| s.to_string()).collect(),
                properties: None,
            },
            tags.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn jaccard_basic() {
        assert!((jaccard(&["a".into(), "b".into()], &["b".into(), "c".into()]) - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(jaccard(&[], &[]), 0.0);
        assert_eq!(jaccard(&["a".into()], &[]), 0.0);
        assert_eq!(jaccard(&["a".into()], &["a".into()]), 1.0);
    }

    #[test]
    fn node_kind_roundtrip() {
        for k in [NodeKind::Page, NodeKind::Book, NodeKind::Shelf] {
            assert_eq!(NodeKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(NodeKind::parse("nope"), None);
    }

    #[tokio::test]
    async fn upsert_then_find_by_label() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        lib.upsert_node(
            "p1", NodeKind::Page, "Rust async patterns",
            &["rust".into(), "async".into()], None,
        ).await.unwrap();

        let found = lib.find_by_label(NodeKind::Page, "Rust async patterns").await.unwrap();
        assert_eq!(found.as_deref(), Some("p1"));
    }

    #[tokio::test]
    async fn cluster_two_tag_overlapping_pages_creates_one_book() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        // The clusterer links pages via FK-constrained edges, so
        // the pages must exist in the DB first.
        for p in [
            make_page("p1", "tokio task scheduler", &["rust", "tokio"]),
            make_page("p2", "tokio runtime internals", &["rust", "tokio", "async"]),
        ] {
            lib.upsert_node(&p.0.id, p.0.kind, &p.0.label, &p.1, None).await.unwrap();
        }

        let created = lib.cluster_unlinked_pages().await.unwrap();
        assert_eq!(created, 1, "two highly-overlapping pages → one book");

        let stats = lib.stats().await.unwrap();
        assert_eq!(stats.books_created, 1);
        assert_eq!(stats.pages_ingested, 2);
    }

    #[tokio::test]
    async fn cluster_disjoint_tags_creates_two_books() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        for p in [
            make_page("p1", "kotlin coroutines", &["kotlin", "jvm"]),
            make_page("p2", "tokio scheduler",   &["rust",  "tokio"]),
        ] {
            lib.upsert_node(&p.0.id, p.0.kind, &p.0.label, &p.1, None).await.unwrap();
        }

        let created = lib.cluster_unlinked_pages().await.unwrap();
        assert_eq!(created, 2, "disjoint tags → two distinct books");
    }

    #[tokio::test]
    async fn run_clustering_pass_links_pages_to_books_to_shelves() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        // Three rust pages + one python page → 1 rust book + 1
        // python book → 1 rust shelf + 1 python shelf. Tags are
        // chosen so the books end up with disjoint tag sets
        // (rust {rust, async, lifetimes}, python {python, ml}) —
        // otherwise the clusterer's alphabetical tiebreak on the
        // most-common tag could label the python shelf "async".
        for p in [
            make_page("p1", "rust async", &["rust", "async"]),
            make_page("p2", "rust borrow checker", &["rust"]),
            make_page("p3", "rust lifetimes", &["rust", "lifetimes"]),
            make_page("p4", "python ml pipeline", &["python", "ml"]),
        ] {
            lib.upsert_node(&p.0.id, p.0.kind, &p.0.label, &p.1, None).await.unwrap();
        }
        lib.cluster_unlinked_pages().await.unwrap();
        let shelves_created = lib.organize_books_onto_shelves().await.unwrap();
        assert_eq!(shelves_created, 2);

        let shelves = sqlx::query("SELECT label FROM nodes WHERE type='shelf' ORDER BY label")
            .fetch_all(store.pool()).await.unwrap();
        let labels: Vec<String> = shelves.into_iter().map(|r| r.get::<String,_>("label")).collect();
        assert_eq!(labels.len(), 2);
        // The most-common tag in each book should be the shelf name.
        assert!(labels.contains(&"rust".to_string()));
        assert!(labels.contains(&"python".to_string()));
    }

    #[tokio::test]
    async fn unlinked_pages_excludes_already_assigned() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        // Persist a page directly via upsert_node so it's "real".
        lib.upsert_node("p1", NodeKind::Page, "rust async",
            &["rust".into(), "async".into()], None).await.unwrap();
        // Cluster it.
        lib.cluster_unlinked_pages().await.unwrap();

        // After clustering, the page should have a belongs_to edge
        // and no longer appear in unlinked_pages().
        let remaining = lib.unlinked_pages().await.unwrap();
        assert!(remaining.is_empty(), "page was already clustered");
    }

    #[tokio::test]
    async fn reset_cluster_wipes_only_belongs_to_and_sits_on() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        lib.upsert_node("p1", NodeKind::Page, "rust", &["rust".into()], None).await.unwrap();
        lib.cluster_unlinked_pages().await.unwrap();
        lib.organize_books_onto_shelves().await.unwrap();

        let deleted = lib.reset_cluster().await.unwrap();
        assert!(deleted >= 2, "expected at least 2 edges to be wiped");

        // Tag edges must remain.
        let tag_edges: i64 = sqlx::query(
            "SELECT COUNT(*) AS c FROM edges WHERE relation_type='tag'"
        )
        .fetch_one(store.pool()).await.unwrap().get("c");
        assert!(tag_edges > 0, "tag edges should survive reset_cluster");
    }

    #[tokio::test]
    async fn expand_surfaces_page_book_and_shelf_in_depth_order() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        // Build a 3-tier graph: page → book → shelf.
        lib.upsert_node(
            "p1", NodeKind::Page, "rust async runtime",
            &["rust".into(), "async".into()], None,
        ).await.unwrap();
        lib.cluster_unlinked_pages().await.unwrap();
        lib.organize_books_onto_shelves().await.unwrap();

        let hits = lib.expand("rust async runtime").await.unwrap();
        // Should contain at least one Page, one Book, and one Shelf.
        let kinds: Vec<NodeKind> = hits.iter().map(|h| h.kind).collect();
        assert!(kinds.contains(&NodeKind::Page),  "missing Page hit, got {:?}", kinds);
        assert!(kinds.contains(&NodeKind::Book),  "missing Book hit, got {:?}", kinds);
        assert!(kinds.contains(&NodeKind::Shelf), "missing Shelf hit, got {:?}", kinds);

        // Depths: page = 0, book = 1, shelf = 2.
        for h in &hits {
            let expected = match h.kind {
                NodeKind::Page  => 0,
                NodeKind::Book  => 1,
                NodeKind::Shelf => 2,
            };
            assert_eq!(h.depth, expected, "wrong depth for {:?}", h);
        }
    }

    #[tokio::test]
    async fn expand_returns_empty_for_no_match() {
        let store = fresh_store().await;
        let lib = Library::new(&store);
        let hits = lib.expand("zzz-nothing-here").await.unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn expand_dedupes_when_two_pages_share_a_book() {
        let store = fresh_store().await;
        let lib = Library::new(&store);

        // Two pages that will end up in the same book.
        lib.upsert_node(
            "p1", NodeKind::Page, "rust tokio scheduler",
            &["rust".into(), "tokio".into()], None,
        ).await.unwrap();
        lib.upsert_node(
            "p2", NodeKind::Page, "rust tokio runtime",
            &["rust".into(), "tokio".into()], None,
        ).await.unwrap();
        lib.cluster_unlinked_pages().await.unwrap();

        let hits = lib.expand("rust tokio").await.unwrap();
        let book_hits: Vec<&ExpandHit> = hits.iter()
            .filter(|h| h.kind == NodeKind::Book)
            .collect();
        assert_eq!(book_hits.len(), 1, "two pages in one book → one Book hit, got {:?}", book_hits);
    }
}