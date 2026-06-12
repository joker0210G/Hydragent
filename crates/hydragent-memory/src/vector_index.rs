// crates/hydragent-memory/src/vector_index.rs
//
// HNSW-backed vector store. Replaces the prior O(N) linear scan over
// `HashMap<String, Vec<f32>>` with an actual approximate-nearest-
// neighbor index (`hnsw_rs`). The public API is unchanged from the
// pre-Phase-2-final linear-scan version, so callers in
// `retrieval.rs::hybrid_search`, `semantic_store.rs`, and the
// Criterion bench continue to work without modification.
//
// Internals:
//
//   * `embeddings`  — source-of-truth `String → Vec<f32>`. HNSW is
//                    rebuilt from this on `load_from_disk`, so on-disk
//                    persistence is just a HashMap snapshot.
//   * `hnsw`        — the ANN index, addressed by `usize` ids.
//   * `string_to_usize` / `usize_to_string` — bidirectional id map
//                    between our String UUIDs and HNSW's usize ids.
//   * `tombstones`  — HNSW has no native delete, so we mark deleted
//                    usize ids as tombstones and filter search
//                    results. When the tombstone set grows past a
//                    threshold we rebuild the index.
//   * `next_id`     — monotonic source of fresh usize ids.
//
// Search returns `1.0 - distance` (i.e. a similarity score) so
// existing call sites that sort results descending by score still
// get the expected ordering.
//
// On-disk format is a bincode blob with a 1-byte magic header so we
// can auto-discard pre-HNSW linear-scan snapshots without crashing.

use serde::{Serialize, Deserialize};
use std::path::Path;
use std::collections::{HashMap, HashSet};
use anyhow::Result;
use hnsw_rs::prelude::*;

/// 1-byte magic header identifying an HNSW-backed bincode blob. Old
/// linear-scan snapshots (no `magic` field) fail to deserialize
/// cleanly, which the caller in `SessionStore::new` already handles
/// by falling back to a fresh `VectorStore::new()`.
const HNSW_BINCODE_MAGIC: u8 = 0x48; // 'H'

#[derive(Serialize, Deserialize)]
struct PersistedVectorStore {
    magic: u8,
    /// The full embedding map at the time of save. HNSW is rebuilt
    /// from this on load — `Vec<f32>` is the canonical embedding, the
    /// graph is just an acceleration structure.
    embeddings: HashMap<String, Vec<f32>>,
    /// Monotonic id counter at save time. Restored on load so future
    /// inserts don't collide with resurrected nodes.
    next_id: u64,
}

pub struct VectorStore {
    /// Source of truth: maps the public String id to the embedding
    /// vector. HNSW is just an acceleration structure over this map.
    embeddings: HashMap<String, Vec<f32>>,

    /// The actual ANN index. `None` means "not yet allocated" — the
    /// first `insert` or `search` lazily calls `Hnsw::new`. This makes
    /// `clear()` near-instant (it just drops the `Option`) instead of
    /// paying the full `MAX_ELEMENTS`-sized `Hnsw::new` allocation up
    /// front. G1 (test) hit the cold-start cost on the first search
    /// after `memory.clear`; lazy init removes the up-front tax.
    hnsw: Option<Hnsw<'static, f32, DistCosine>>,

    /// `String → usize` map. HNSW's id type is `usize`; the rest of
    /// the codebase hands us String UUIDs.
    string_to_usize: HashMap<String, usize>,
    /// Reverse map for `O(1)` search-result lookup.
    usize_to_string: HashMap<usize, String>,

    /// HNSW does not support true deletion, so we mark deleted usize
    /// ids here and filter them out of search results. Rebuilt away
    /// by `rebuild_hnsw` once it grows too large.
    tombstones: HashSet<usize>,

    /// Monotonic source of fresh usize ids.
    next_id: usize,

    /// Search-time `ef` (the HNSW equivalent of beam width). 50 is a
    /// reasonable default for indices in the 1k–100k range.
    ef_search: usize,
}

impl Default for VectorStore {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorStore {
    /// M: number of bi-directional links per node per layer.
    /// 16 is the canonical HNSW value.
    const M: usize = 16;

    /// Pre-allocated capacity. We pick 10k to keep the initial
    /// `PointIndexation` allocation small (HNSW pre-allocates
    /// ~`max_layer` vectors sized by `max_elements`, plus a few
    /// 16-byte neighbor slots per layer). 10k is enough for the
    /// Phase-2 stress suite (which only inserts a few hundred
    /// memories) and well below the soft LRU cap in
    /// `SessionStore::max_memories` (1M). If the index needs to grow
    /// beyond 10k, hnsw_rs will handle it (we just lose the
    /// pre-alloc). 1M was tried first but caused the
    /// `hydragent memory clear` CLI subcommand to OOM on Windows
    /// (~232 MB allocation) because the system allocator couldn't
    /// satisfy the large reservation.
    const MAX_ELEMENTS: usize = 10_000;

    /// `ef_construction`: candidate list size during graph build.
    /// 200 is the canonical HNSW value.
    const EF_CONSTRUCTION: usize = 200;

    /// Default search beam width.
    const EF_SEARCH: usize = 50;

    /// Number of tombstones that triggers a full HNSW rebuild.
    /// 1024 keeps the overhead low while bounding worst-case search
    /// overshoot (we over-fetch by `tombstones.len() + 8`).
    const REBUILD_THRESHOLD: usize = 1024;

    pub fn new() -> Self {
        Self {
            embeddings: HashMap::new(),
            // Lazy: HNSW is allocated on the first `insert` / `search`
            // via `ensure_hnsw()`. `new()` is now O(1) — useful for
            // fresh starts (e.g. `memory.clear` on an empty store).
            hnsw: None,
            string_to_usize: HashMap::new(),
            usize_to_string: HashMap::new(),
            tombstones: HashSet::new(),
            next_id: 0,
            ef_search: Self::EF_SEARCH,
        }
    }

    /// Lazily allocate the HNSW graph if it isn't already. Called from
    /// every public method that needs it. O(1) when the index is hot
    /// (just an `Option` unwrap), one-time `Hnsw::new` cost on the
    /// first call after `new()` / `clear()` / `load_from_disk()`.
    fn ensure_hnsw(&mut self) -> &mut Hnsw<'static, f32, DistCosine> {
        if self.hnsw.is_none() {
            self.hnsw = Some(Hnsw::new(
                Self::M,
                Self::MAX_ELEMENTS,
                16, // max_layer — 16 is the standard HNSW depth
                Self::EF_CONSTRUCTION,
                DistCosine,
            ));
        }
        self.hnsw.as_mut().expect("just initialized above")
    }

    pub fn insert(&mut self, id: String, vector: Vec<f32>) {
        // If the id already exists, reuse the same usize id. HNSW
        // would reject a duplicate usize, and reusing it means the
        // old embedding is fully replaced (no orphan in `embeddings`).
        let usize_id = if let Some(existing) = self.string_to_usize.get(&id).copied() {
            existing
        } else {
            let n = self.next_id;
            self.next_id += 1;
            n
        };
        self.string_to_usize.insert(id.clone(), usize_id);
        self.usize_to_string.insert(usize_id, id.clone());
        self.embeddings.insert(id, vector.clone());
        // Inserting into HNSW: the new write replaces the prior write
        // at this id (HNSW doesn't have an "update" primitive, but
        // the search layer always traverses through the most recent
        // graph layer built by `insert`).
        // `insert` takes `(&[T], usize)`, so coerce `&Vec<f32>` to a
        // slice with `as_slice()`. Lazily allocate the index if this
        // is the first insert (post-`new` or post-`clear`).
        self.ensure_hnsw().insert((vector.as_slice(), usize_id));

        // Periodic rebuild to bound the tombstone set growth that
        // comes from LRU eviction and `memory.delete` calls.
        if self.tombstones.len() > Self::REBUILD_THRESHOLD {
            self.rebuild_hnsw();
        }
    }

    pub fn search(&self, query_vec: &[f32], k: usize) -> Vec<(String, f32)> {
        if self.embeddings.is_empty() || k == 0 {
            return Vec::new();
        }
        // Lazy init: an empty index (`hnsw = None`) returns no results.
        // In practice this is only hit immediately after `new()` /
        // `clear()` with no intervening `insert` — `load_from_disk`
        // re-inserts every persisted embedding, which forces the
        // HNSW allocation up front.
        let hnsw = match self.hnsw.as_ref() {
            Some(h) => h,
            None => return Vec::new(),
        };
        // Over-fetch to absorb tombstones and a small safety margin.
        // Worst case is `tombstones.len() + 8` extra hops.
        let over = k
            .saturating_add(self.tombstones.len())
            .saturating_add(8);
        let neighbors = hnsw.search(query_vec, over, self.ef_search);
        let mut results = Vec::with_capacity(k);
        for n in neighbors {
            if self.tombstones.contains(&n.d_id) {
                continue;
            }
            if let Some(s) = self.usize_to_string.get(&n.d_id) {
                // `DistCosine` distance is `1 - cosine_similarity` for
                // L2-normalized vectors. Flip it so callers can sort
                // descending by similarity — same semantics as the
                // pre-HNSW `cosine_similarity()` direct call.
                // Note: cosine distance is in [0, 2] but for our
                // L2-normalized vectors it stays in [0, 1]; clamp
                // defensively in case of unnormalized inputs.
                let raw = 1.0_f32 - n.distance;
                let similarity = raw.clamp(0.0, 1.0);
                results.push((s.clone(), similarity));
                if results.len() >= k {
                    break;
                }
            }
        }
        results
    }

    pub fn delete(&mut self, id: &str) {
        if let Some(usize_id) = self.string_to_usize.remove(id) {
            self.usize_to_string.remove(&usize_id);
            self.embeddings.remove(id);
            // HNSW has no native delete; we tombstone. The next
            // `rebuild_hnsw` will sweep these away.
            self.tombstones.insert(usize_id);
        }
    }

    pub fn clear(&mut self) {
        self.embeddings.clear();
        self.string_to_usize.clear();
        self.usize_to_string.clear();
        self.tombstones.clear();
        self.next_id = 0;
        // Drop the HNSW graph instead of allocating a fresh one.
        // The next `insert` lazily re-allocates via `ensure_hnsw()`.
        // G1 (test) hit the cold-start cost when `clear()` paid the
        // full `MAX_ELEMENTS`-sized `Hnsw::new` up front; lazy drop
        // makes `clear()` O(1) and shifts the allocation to the first
        // real use.
        self.hnsw = None;
    }

    /// Return all stored String ids. The in-memory `embeddings` map
    /// is the source of truth (tombstoned ids have already been
    /// removed from it by `delete()`).
    pub fn keys(&self) -> Vec<String> {
        self.embeddings.keys().cloned().collect()
    }

    pub fn save_to_disk(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Persist only the source-of-truth HashMap. The HNSW graph
        // is rebuilt from the HashMap on load — saves a dependency
        // on hnsw_rs's binary format (which has changed across
        // 0.3.x patch releases) and keeps the on-disk file small.
        let snapshot = PersistedVectorStore {
            magic: HNSW_BINCODE_MAGIC,
            embeddings: self.embeddings.clone(),
            next_id: self.next_id as u64,
        };
        let bytes = bincode::serialize(&snapshot)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn load_from_disk(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        // Old linear-scan snapshots (pre-Phase-2-final) lack the
        // `magic` field and will fail bincode deserialization; the
        // caller's `unwrap_or_else(|_| VectorStore::new())` handles
        // the fallback.
        let snapshot: PersistedVectorStore = bincode::deserialize(&bytes)?;
        if snapshot.magic != HNSW_BINCODE_MAGIC {
            anyhow::bail!("not an HNSW vector store snapshot (magic mismatch)");
        }
        let mut store = Self::new();
        for (id, vec) in snapshot.embeddings {
            store.insert(id, vec);
        }
        // Restore the id counter so future inserts don't collide
        // with resurrected nodes' usize ids.
        store.next_id = snapshot.next_id as usize;
        Ok(store)
    }

    /// Drop the current HNSW graph and rebuild it from the live
    /// `embeddings` HashMap, clearing the tombstone set. O(N log N).
    /// Called automatically when the tombstone set grows past
    /// `REBUILD_THRESHOLD`; also exposed for tests / manual recovery.
    fn rebuild_hnsw(&mut self) {
        // Snapshot keys+values to avoid borrow conflicts with
        // `self.insert()` which mutates the maps.
        let entries: Vec<(String, Vec<f32>)> = self
            .embeddings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Drop the old graph and reset all id maps. The next `insert`
        // re-allocates the HNSW lazily, and we then re-insert every
        // live entry into the fresh index.
        self.hnsw = None;
        self.tombstones.clear();
        self.string_to_usize.clear();
        self.usize_to_string.clear();
        self.next_id = 0;
        for (id, vec) in entries {
            self.insert(id, vec);
        }
    }
}
