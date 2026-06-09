use crate::SessionStore;
use hydragent_types::MemoryDocument;
use anyhow::Result;
use std::collections::{HashMap, HashSet};

pub async fn hybrid_search(
    query: &str,
    limit: usize,
    store: &SessionStore,
) -> Result<Vec<MemoryDocument>> {
    // 1. Run FTS5 search (keyword rank)
    let fts_memories = store.search_memories_fts(query).await.unwrap_or_default();

    // 2. Run Vector similarity search
    let mut vector_hits = Vec::new();
    if let Ok(embedder) = store.get_embedder().await {
        if let Ok(query_vector) = embedder.embed_text(query) {
            let vs = store.vector_store.lock().unwrap();
            let hits = vs.search(&query_vector, 20);
            vector_hits = hits;
        }
    }

    // 3. Build rank maps (1-indexed)
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

    // 4. Perform Reciprocal Rank Fusion (RRF)
    const RRF_K: f64 = 60.0;
    let mut all_ids = HashSet::new();
    for id in fts_ranks.keys() {
        all_ids.insert(id.clone());
    }
    for id in vector_ranks.keys() {
        all_ids.insert(id.clone());
    }

    let mut scored_docs = Vec::new();
    for id in all_ids {
        let mut score = 0.0;
        if let Some(rank) = fts_ranks.get(&id) {
            score += 1.0 / (RRF_K + *rank as f64);
        }
        if let Some(rank) = vector_ranks.get(&id) {
            score += 1.0 / (RRF_K + *rank as f64);
        }
        scored_docs.push((id, score));
    }

    // Sort descending by RRF score
    scored_docs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored_docs.truncate(limit);

    // 5. Hydrate to MemoryDocument structures
    let mut final_docs = Vec::new();
    for (id, rrf_score) in scored_docs {
        if let Some(mem) = store.get_memory(&id).await? {
            final_docs.push(MemoryDocument {
                id: mem.id,
                content: mem.content,
                timestamp: mem.timestamp,
                importance: mem.importance,
                rrf_score,
            });
        }
    }

    Ok(final_docs)
}
