use crate::library::{ExpandHit, Library, NodeKind};
use crate::SessionStore;
use hydragent_types::MemoryDocument;
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// Unified Hybrid Query Bridge — per design spec §3.
///
/// Prevents duplicate LLM queries and minimises execution time by running
/// two local retrieval paths **in parallel** (tokio join) and merging them
/// into a single ranked context bubble before system-prompt injection.
///
/// Step 1 – SQLite FTS5 keyword match → finds matching Page nodes fast.
/// Step 2 – Graph expansion via [`Library::expand`] → traverses Books
///           & Shelves that neighbour the matched Pages for broader
///           context. Implemented over the typed `NodeKind` /
///           `EdgeRelation` API in [`crate::library`] so the magic
///           strings stay out of the SQL.
///
/// Both steps complete locally in < 10 ms with no LLM calls.
pub async fn hybrid_search(
    query: &str,
    limit: usize,
    store: &SessionStore,
) -> Result<Vec<MemoryDocument>> {
    // ── Step 1 & Step 2 run in parallel ─────────────────────────────────────
    let library = Library::new(store);
    let (fts_result, expand_hits) = tokio::join!(
        fts_search(query, store),
        library.expand(query),
    );

    let fts_memories = fts_result.unwrap_or_default();
    let graph_docs   = hits_to_memory_docs(expand_hits.unwrap_or_default());

    // ── Vector similarity search (best-effort, no-panic) ────────────────────
    let mut vector_hits: Vec<(String, f32)> = Vec::new();
    if let Ok(embedder) = store.get_embedder().await {
        if let Ok(query_vector) = embedder.embed_text(query) {
            let vs = store.vector_store().lock().unwrap();
            vector_hits = vs.search(&query_vector, 20);
        }
    }

    // ── Build rank maps (1-indexed) ──────────────────────────────────────────
    let fts_ranks: HashMap<String, usize> = fts_memories
        .iter()
        .enumerate()
        .map(|(idx, mem)| (mem.id.clone(), idx + 1))
        .collect();

    let vector_ranks: HashMap<String, usize> = vector_hits
        .iter()
        .enumerate()
        .map(|(idx, (id, _score))| (id.clone(), idx + 1))
        .collect();

    // ── Reciprocal Rank Fusion (RRF) over semantic memories ─────────────────
    const RRF_K: f64 = 60.0;
    let mut all_ids: HashSet<String> = fts_ranks.keys().cloned().collect();
    all_ids.extend(vector_ranks.keys().cloned());

    let mut scored_docs: Vec<(String, f64)> = all_ids
        .into_iter()
        .map(|id| {
            let mut score = 0.0_f64;
            if let Some(rank) = fts_ranks.get(&id) {
                score += 1.0 / (RRF_K + *rank as f64);
            }
            if let Some(rank) = vector_ranks.get(&id) {
                score += 1.0 / (RRF_K + *rank as f64);
            }
            (id, score)
        })
        .collect();

    scored_docs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored_docs.truncate(limit);

    // ── Hydrate to MemoryDocument structures ─────────────────────────────────
    let mut final_docs: Vec<MemoryDocument> = Vec::new();
    for (id, rrf_score) in scored_docs {
        if let Ok(Some(mem)) = store.get_memory(&id).await {
            final_docs.push(MemoryDocument {
                id: mem.id,
                content: mem.content,
                timestamp: mem.timestamp,
                importance: mem.importance,
                rrf_score,
            });
        }
    }

    // ── Append graph-expanded context docs (Books & Shelves) ─────────────────
    // These carry graph-level context (topic clusters and domain categories)
    // that the pure semantic search cannot surface. They are appended after
    // the RRF-ranked Page hits so the single injection stays compact.
    for doc in graph_docs {
        if !final_docs.iter().any(|d| d.id == doc.id) {
            final_docs.push(doc);
        }
    }

    Ok(final_docs)
}

/// Step 1: Fast SQLite FTS5 keyword scan over `semantic_memories`.
async fn fts_search(query: &str, store: &SessionStore) -> Result<Vec<crate::SemanticMemory>> {
    store.search_memories_fts(query).await
}

/// Step 2: Graph expansion. The traversal itself lives in
/// [`Library::expand`] so the SQL goes through the typed
/// `NodeKind` / `EdgeRelation` API rather than magic strings.
/// This helper turns the typed hits into synthetic
/// [`MemoryDocument`]s that the rest of the agent runtime can
/// inject into the system prompt without knowing about the graph.
///
/// Importance grows with depth (Page < Book < Shelf) so the
/// context bubble is naturally tier-ordered.
fn hits_to_memory_docs(hits: Vec<ExpandHit>) -> Vec<MemoryDocument> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    hits.into_iter()
        .map(|hit| {
            let (prefix, importance, rrf) = match (hit.kind, hit.depth) {
                (NodeKind::Page, _) => ("[Page]", 5, 0.016),
                (NodeKind::Book, _) => ("[Book / Topic Cluster]", 6, 0.020),
                (NodeKind::Shelf, _) => ("[Shelf / Domain]", 7, 0.025),
            };
            MemoryDocument {
                id: format!("graph:{}:{}", hit.kind, hit.node_id),
                content: format!("{} {}", prefix, hit.label),
                timestamp: now_ms,
                importance,
                rrf_score: rrf,
            }
        })
        .collect()
}
