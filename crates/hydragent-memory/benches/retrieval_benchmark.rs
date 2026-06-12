// crates/hydragent-memory/benches/retrieval_benchmark.rs
//
// Criterion bench for `hybrid_search` and the underlying BM25 + vector paths.
//
//   * `hybrid_search`     — full FTS5 ∪ cosine RRF (the hot path used by
//                           `react_loop.rs`).
//   * `vector_search`     — raw in-memory HNSW (hnsw_rs) ANN search
//                           — the post-Phase-2-final `vector_index.rs`
//                           implementation. Sub-linear in N.
//   * `fts_search`        — raw FTS5 BM25 lookup.
//
// We populate the store with two corpus sizes (1 000 and 10 000 facts)
// so the bench captures the linear growth of the vector scan and the
// sub-linear growth of FTS5.
//
// Run with:
//   cargo bench -p hydragent-memory --bench retrieval_benchmark
//
// Reports land in `target/criterion/`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, black_box};
use hydragent_memory::{SessionStore, hybrid_search};
use std::time::Duration;
use uuid::Uuid;

/// In-memory SQLite so each run is hermetic and fast.
const DB_URL: &str = "file:bench_memdb?mode=memory&cache=shared";

/// Build a fresh store and seed it with `n` synthetic facts whose
/// `content` field is a short technical sentence so FTS5 actually has
/// terms to work with.
async fn seed(n: usize) -> SessionStore {
    let store = SessionStore::new(DB_URL).await.expect("open db");

    // Wipe the vector store to be safe (the in-memory SQLite pool is
    // shared but the vector store is a `Mutex` per `SessionStore`).
    {
        let mut vs = store.vector_store().lock().unwrap();
        vs.clear();
    }

    // We embed everything off-line by calling `insert_memory`, which
    // routes through the real embedder. To keep the bench self-contained
    // we use a synthetic id; the content is short and varied enough
    // that embedding dominates nothing.
    let topics = [
        "rust async tokio scheduling",
        "candle transformer inference",
        "sqlx sqlite wal mode",
        "json-rpc tcp framer",
        "hnsw cosine similarity",
        "rrf fusion rank aggregation",
        "bm25 full text search",
        "context window token budget",
        "channel adapter webhook bus",
        "page session memory consolidation",
    ];

    for i in 0..n {
        let id = Uuid::new_v4().to_string();
        let topic = topics[i % topics.len()];
        let content = format!(
            "Fact #{i}: {topic} — synthetic seed for retrieval benchmark, \
             index entry {i} of {n}."
        );
        // Importance wobbles 1..=5 so the eviction sweep is never empty
        // if a downstream caller ever enables the cap mid-bench.
        let importance = ((i % 5) + 1) as i64;
        let _ = store
            .insert_memory(&id, Some("bench-page"), &content, importance, &[])
            .await;
    }
    store
}

/// Count a single search call under the criterion timer. Wrapped in
/// `block_in_place` so we can run the future on the current thread
/// without spinning a runtime per call.
async fn run_hybrid(store: &SessionStore, query: &str) {
    let _docs = hybrid_search(query, 10, store).await.expect("hybrid");
}

async fn run_vector(store: &SessionStore, query: &str) {
    let embedder = store.get_embedder().await.expect("embedder");
    let q = embedder.embed_text(query).expect("embed");
    let vs = store.vector_store().lock().unwrap();
    let _hits = vs.search(&q, 10);
}

async fn run_fts(store: &SessionStore, query: &str) {
    let _hits = store.search_memories_fts(query).await.expect("fts");
}

/// `c.bench_function` cannot await directly, so we wrap the async work
/// in a small runtime helper. The runtime is constructed once per call
/// using a `block_on` style shim — cheap for short tasks.
fn bench_with<F>(c: &mut Criterion, name: &str, sizes: &[usize], mut f: F)
where
    F: FnMut(&SessionStore, &str) + Send,
{
    let mut group = c.benchmark_group(name);
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(20);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");

    for &n in sizes {
        let store = rt.block_on(seed(n));
        let query = "rust async tokio";

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &_n| {
            b.iter(|| {
                // The call itself is sync (we capture the future into
                // a local function that runs on the runtime).
                let s = &store;
                let q = query;
                f(s, q);
                black_box(());
            });
        });
    }
    group.finish();
}

fn hybrid_bench(c: &mut Criterion) {
    bench_with(c, "hybrid_search", &[1_000, 10_000], |store, q| {
        // The async fns need a runtime; build one for the duration of
        // this call only. This is what criterion actually times.
        let _ = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_hybrid(store, q));
    });
}

fn vector_bench(c: &mut Criterion) {
    bench_with(c, "vector_search_hnsw", &[1_000, 10_000], |store, q| {
        let _ = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_vector(store, q));
    });
}

fn fts_bench(c: &mut Criterion) {
    bench_with(c, "fts_search_bm25", &[1_000, 10_000], |store, q| {
        let _ = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_fts(store, q));
    });
}

criterion_group!(benches, hybrid_bench, vector_bench, fts_bench);
criterion_main!(benches);
