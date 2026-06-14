# Phase 2: Hierarchical Memory & Retrieval-Augmented Thinking (Weeks 7–10)

> **Timeline**: Weeks 7–10
> **Theme**: Transform Hydragent from a stateless chat loop into a **continuously learning entity** — a multi-tiered memory system that persists facts, preferences, and project state indefinitely. Retrieval is hybrid (BM25 + vector), context injection is automatic and token-budget-aware, and a nightly "Dreaming" background worker compacts raw conversation logs into dense, searchable knowledge.

> ## ✅ Implementation Status — Implemented (subset, as of June 2026)
> 
> Cross-checked against [`doc/STATE.md`](../STATE.md) at `git rev 3d99366` (June 2026).
> 
> **What is live:**
> - **SQLite-backed memory** is live in `hydragent-memory` (semantic store, session log, vector index, retrieval, context injector).
> - **Vector index** is a **linear scan** over `HashMap<String, Vec<f32>>` in `crates/hydragent-memory/src/vector_index.rs`. **NOT HNSW** — the spec called for `hnsw_rs` but that was never integrated. The 100k-fact / 10ms latency target is not achievable with the current code. Persistence is via bincode to `vectors.bin`; on disk it's just a serialized HashMap. See §5.4 for the actual code and §3.4 for the original design rationale (which was not implemented).
> - **Hybrid retrieval (BM25 + vector + RRF)** is wired and exposed through the `memory_search` tool AND the bus RPC `memory.search` (added 2026-06-12).
> - **Memory tools** are live: `memory_store`, `memory_search`, `memory_forget` (plus `soul` and `user_profile`).
> - **Local embedder** (`hydragent-embed`) downloads and runs `all-MiniLM-L6-v2` via Candle; model artifacts are committed to `data/models/`.
> - **Dreaming pipeline** is **scaffolded but not running nightly**. The background worker entrypoint exists; consolidation logic is minimal.
> - **Type note**: `MemoryDocument.importance` is an `i64` (integer), not the `f32` shown in some Phase 2 diagrams.
> - **`soul` tool** (a.k.a. "Standing Orders") lives in `hydragent-tools` (file: `crates/hydragent-tools/src/standing_orders.rs` — file name is historical, the registered tool name is `soul`). Reads / writes `config/SOUL.md`.
> 
> **Not yet built / not exercised:**
> - The Dreaming worker has not been observed to actually consolidate logs in normal runs.
> - LRU eviction policy is described but not enforced.
> - The 100k-fact / 10ms retrieval target is unverified; no benchmark suite ships in `benches/`.

---

> **Doc vs Code Reality (as of 2026-06-12):** Stress-test sweep is GREEN
> (21/21, see [`PHASE_2_FINAL_REPORT.md`](../archive/phases/PHASE_2_FINAL_REPORT.md)). Several
> components in this file are **aspirational** rather than the current
> implementation. Specifically:
>
> - **§5.2 (DB Schema)** — the FTS5 table here is shown as external-content
>   with `tokenize='trigram'`. The real code uses a **standard** (non-external)
>   FTS5 table with the default `unicode61` tokenizer. The triggers ARE
>   present (good!), but the trigger names are `fts_insert` / `fts_update` /
>   `fts_delete` (not `sm_ai` / `sm_au` / `sm_ad`).
> - **§5.4 (HNSW Vector Index)** — shows `hnsw_rs::Hnsw<f32, DistCosine>`.
>   Reality is a **linear scan** over `HashMap<String, Vec<f32>>` in
>   `crates/hydragent-memory/src/vector_index.rs`. The 100k-fact / 10ms
>   target is **not achievable** with the current implementation.
>   Real HNSW integration is deferred to a later phase.
> - **§5.9 (Standing Orders)** — code shows `load_standing_orders()` reading
>   `config/standing_orders.md`. Reality: the tool is named **`soul`**
>   (struct `SoulTool` in `crates/hydragent-tools/src/standing_orders.rs`)
>   and writes **`config/SOUL.md`**.
> - **§5.7.3 (Background Worker)** — accurate, but the `enable_dreaming`
>   default is `true` in code (`config.rs:81`), not a `false`/unknown
>   default as the doc table implies. Verified via Phase 2 test I1.
> - **§10 (Risks)** — FTS5 is **not** using trigram in code; trigram
>   would have ~3× storage overhead which we don't actually pay.
>
> The full divergence table lives in [`PHASE_2_FINAL_REPORT.md`](../archive/phases/PHASE_2_FINAL_REPORT.md)
> §"Doc-vs-code divergences still present".

## 📋 Table of Contents

1. [Phase Goals & Exit Criteria](#1-phase-goals--exit-criteria)
2. [Directory & Workspace Layout Changes](#2-directory--workspace-layout-changes)
3. [Technology Decisions](#3-technology-decisions)
4. [Week-by-Week Breakdown](#4-week-by-week-breakdown)
5. [Component Specifications](#5-component-specifications)
   - 5.1 [Memory Architecture & Tier Design](#51-memory-architecture--tier-design)
   - 5.2 [Database Schema & Migrations](#52-database-schema--migrations)
   - 5.3 [Local Vector Embedding Pipeline](#53-local-vector-embedding-pipeline)
   - 5.4 [HNSW Vector Index](#54-hnsw-vector-index)
   - 5.5 [Hybrid Retrieval & Reciprocal Rank Fusion (RRF)](#55-hybrid-retrieval--reciprocal-rank-fusion-rrf)
   - 5.6 [Context Injection & Token Budget Manager](#56-context-injection--token-budget-manager)
   - 5.7 [Dreaming Pipeline (Memory Consolidation)](#57-dreaming-pipeline-memory-consolidation)
   - 5.8 [Memory Tools (memory_store, memory_search, memory_forget)](#58-memory-tools)
   - 5.9 [Standing Orders Module](#59-standing-orders-module)
   - 5.10 [Memory Lifecycle & LRU Eviction](#510-memory-lifecycle--lru-eviction)
6. [Built-in Tools (Phase 2 Additions)](#6-built-in-tools-phase-2-additions)
7. [Configuration & Environment](#7-configuration--environment)
8. [Testing Strategy](#8-testing-strategy)
9. [Performance Targets](#9-performance-targets)
10. [Risks & Mitigations](#10-risks--mitigations)
11. [Definition of Done](#11-definition-of-done)

---

## 1. Phase Goals & Exit Criteria

Phase 2 makes Hydragent's memory **infinite, autonomous, and searchable**. The agent must remember facts across sessions without user intervention, recall them using semantic similarity, and automatically consolidate verbose logs into compact knowledge. Nothing is handed off from Phase 1 — all Phase 1 tests must remain green.

### Hard Goals (must achieve before Phase 3)

| # | Goal | Validation |
|---|---|---|
| G1 | Cross-session fact recall without explicit user prompting | Integration test: agent recalls `"My dog is named Barnaby"` from prior session without invoking `memory_search` |
| G2 | Hybrid search (BM25 + vector) within 50 ms for a 10,000-fact database | Criterion benchmark in `benches/retrieval_benchmark.rs` |
| G3 | Local embedding generation < 30 ms per query on CPU | Unit test timing with `std::time::Instant` |
| G4 | Dreaming worker consolidates raw messages into facts without blocking event bus | `tokio::spawn` confirmed in separate thread pool; EventBus accepts messages during active dream cycle |
| G5 | `memory_store`, `memory_search`, `memory_forget` tools all functional in ReAct loop | Integration test: full ReAct turn for each tool with expected output verified |
| G6 | Context injection respects `MEMORY_CONTEXT_TOKEN_LIMIT` — never injects more tokens than configured | Unit test: inject 200 memories with a 500-token limit; assert injected block ≤ 500 tokens |
| G7 | FTS5 triggers keep keyword index perfectly in sync — no orphaned rows | Unit test: insert/update/delete 100 records; verify FTS row count matches main table row count |
| G8 | `SOUL.md` file is parsed and injected at every session start as immutable system context | Integration test: `config/SOUL.md` content appears in every LLM system prompt regardless of user input. **Note**: the doc historically called this file `standing_orders.md` and the tool `standing_orders`; reality is `SOUL.md` and the `soul` tool. See §5.9. |
| G9 | HNSW index persists to disk on graceful shutdown; reloads on next startup | Test: insert 1,000 vectors, restart process, assert all 1,000 are searchable |

### Soft Goals (target but not blocking)

- `memory_search` tool shows the user a confidence score alongside each recalled fact
- The Dreaming worker reports its statistics via `tracing::info!` (facts extracted, tokens processed)
- The agent auto-tags memories with categories (`personal`, `technical`, `preference`, `project_state`) from the extraction LLM
- `./hydragent memory list` CLI subcommand to inspect the semantic_memories table
- `./hydragent memory clear` CLI subcommand to wipe all semantic memories (with confirmation prompt)

---

## 2. Directory & Workspace Layout Changes

Phase 2 significantly expands `crates/hydragent-memory` and introduces `crates/hydragent-embed` for the local embedding pipeline.

```
hydragent/
│
├── crates/
│   │
│   ├── hydragent-core/                       # UPDATED
│   │   └── src/
│   │       ├── main.rs                        # UPDATED: dreaming worker spawn, embedder init
│   │       ├── orchestrator.rs               # UPDATED: pre-prompt context injection
│   │       ├── react_loop.rs                 # No changes required
│   │       └── dream.rs                      # NEW: run_dream_cycle() background worker
│   │
│   ├── hydragent-memory/                     # HEAVILY UPDATED
│   │   ├── Cargo.toml                        # UPDATED: adds sqlx FTS5 feature, bincode (NOT hnsw_rs — that was never added)
│   │   └── src/
│   │       ├── lib.rs                        # UPDATED: re-exports all memory APIs
│   │       ├── session_store.rs              # UPDATED: requires_consolidation column support
│   │       ├── semantic_store.rs             # NEW: CRUD for semantic_memories table
│   │       ├── models.rs                     # NEW: SemanticMemory, MemoryConsolidationJob structs
│   │       ├── retrieval.rs                  # NEW: hybrid_search(), RRF algorithm
│   │       ├── context_injector.rs           # NEW: build_system_prompt_with_memory()
│   │       ├── vector_index.rs              # NEW: VectorStore (linear-scan HashMap; HNSW not implemented)
│   │       └── dream.rs                      # NEW: run_dream_cycle() background worker (defined here, not in hydragent-core)
│   │
│   ├── hydragent-embed/                      # NEW CRATE: local embedding model
│   │   ├── Cargo.toml                        # candle-core, candle-transformers, tokenizers
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── embedder.rs                   # LocalEmbedder struct + embed_text()
│   │       ├── model_downloader.rs           # On-demand model download from HuggingFace
│   │       └── pooling.rs                   # Mean pooling + L2 normalization helpers
│   │
│   ├── hydragent-tools/                      # UPDATED
│   │   └── src/
│   │       ├── memory_store.rs              # NEW tool: MemoryStoreTool
│   │       ├── memory_search.rs             # NEW tool: MemorySearchTool
│   │       ├── memory_forget.rs             # NEW tool: MemoryForgetTool
│   │       └── standing_orders.rs           # NEW: SoulTool (registered tool name: "soul") — file name is historical
│   │
│   └── hydragent-types/                      # UPDATED
│       └── src/
│           └── lib.rs                        # UPDATED: MemoryDocument, ExtractionResult types
│
├── data/
│   ├── sessions/                             # Existing Phase 1 SQLite session files
│   ├── models/                              # NEW: downloaded embedding model files
│   │   ├── all-MiniLM-L6-v2.safetensors    # ~90 MB embedding model (downloaded at first run)
│   │   ├── tokenizer.json                   # HuggingFace tokenizer config
│   │   └── config.json                      # BERT model config
│   └── vectors.bin                          # NEW: serialized VectorStore (HashMap, not HNSW)
│
├── config/
│   ├── SOUL.md                               # Persistent behavioral rules (a.k.a. "Standing Orders")
│   └── USER.md                               # Persistent user profile
│
├── migrations/
│   ├── 001_initial.sql                       # Existing Phase 1 migration
│   └── 002_hierarchical_memory.sql           # NEW Phase 2 migration
│
├── benches/
│   └── retrieval_benchmark.rs               # NEW: Criterion benchmark for hybrid search latency
│
└── tests/
    └── integration/
        ├── memory_amnesia_test.sh            # NEW: cross-session recall integration test
        └── dreaming_test.rs                  # NEW: consolidation pipeline test
```

---

## 3. Technology Decisions

> **Team consensus (revised 2026-06-12)**: local-first embedding, SQLite FTS5 for keywords. The `hnsw_rs` decision was not implemented — see §3.4 for what shipped instead. No hosted vector databases (Pinecone, Weaviate, Qdrant) in Phase 2 — all data stays on the user's machine.

---

### 3.1 Language Roles in Phase 2

| Component | Language | Rationale |
|---|---|---|
| Memory CRUD, retrieval, RRF algorithm | **Rust** | Performance critical; called on every user turn |
| Local embedding model runner | **Rust (`candle`)** | Pure Rust ML — no Python dependency, no libtorch |
| Dreaming background worker | **Rust (Tokio)** | Runs in the same process; shared DB pool via `Arc` |
| Standing Orders parser | **Rust** | Simple Markdown file parse — no Python needed |
| Model download helper | **Rust (reqwest)** | Download from HuggingFace Hub with progress bar |

---

### 3.2 Embedding Engine Decision

Three candidates were evaluated for the local embedding model:

| Engine | Crate | Pros | Cons | Decision |
|---|---|---|---|---|
| **candle** (HuggingFace) | `candle-core`, `candle-transformers` | Pure Rust; CUDA/Metal optional; Safetensors native; actively maintained | API churn in early versions | ✅ **Chosen** |
| **rust-bert** | `rust-bert` | Feature-rich | Requires `libtorch` (C++ runtime) → complex deployment | ❌ Rejected |
| **ort (ONNX Runtime)** | `ort` | Very fast inference | Requires `.onnx` model format; complex native dependencies | ❌ Rejected |

**Target model**: `sentence-transformers/all-MiniLM-L6-v2`
- **384-dimensional** output vectors — compact yet highly accurate
- **22.7 MB** in `f16` quantization — fits entirely in L3 cache
- **Sentence-transformers benchmark**: 58.8 on MTEB — excellent for short fact retrieval

---

### 3.3 Why SQLite FTS5 (not Tantivy)?

| Factor | SQLite FTS5 | Tantivy |
|---|---|---|
| **Dependencies** | Already in our `sqlx` dependency — zero new deps | New crate, 2.5 MB compiled size |
| **Consistency** | Atomic with the main `semantic_memories` table via triggers | Separate index, requires manual sync |
| **Setup complexity** | 4 SQL lines to create virtual table + triggers | Custom indexing pipeline |
| **Performance** | Trigram tokenizer: sub-2ms for 100k rows | Marginally faster at 1M+ rows |
| **Phase 2 scale** | A personal agent has < 100k facts. FTS5 is more than sufficient | Overkill for Phase 2 |

**Decision**: SQLite FTS5 with `tokenize='trigram'` for Phase 2. Tantivy is the Phase 5+ migration path if we need 10M+ memories.

---

### 3.4 Why HNSW over Linear Scan?

For N facts, linear scan is O(N). HNSW is O(log N) for search. At 10,000 facts:

| Approach | Search time (10k facts, 384 dims) | Memory |
|---|---|---|
| Linear scan (`f32` dot product) | ~8 ms | ~14 MB (10k × 384 × 4B) |
| HNSW (M=16, ef=32) | ~0.3 ms | ~25 MB (graph overhead) |
| LanceDB (disk-based) | ~15 ms (disk I/O) | ~0 MB RAM |

**Decision**: In-memory HNSW via `hnsw_rs` for Phase 2. If the user's memory grows beyond 100k facts, Phase 5 will migrate to LanceDB disk-based storage.

---

### 3.5 Reciprocal Rank Fusion vs. Linear Combination

Why not just add `bm25_score + cosine_similarity`?

- BM25 scores are unbounded positive numbers (a score of 42.7 is "good", but what is "good"?)
- Cosine similarity is bounded [0, 1]
- Adding them directly makes BM25 dominate simply due to scale

RRF normalizes by *rank position* instead of raw score. It's been shown (Cormack et al., 2009) to consistently outperform linear combination for hybrid retrieval. The formula `1 / (k + rank)` is rank-agnostic — it only cares about position. With `k=60`, ranks 1–10 contribute 99% of the score; rank 100 contributes only 1%.

---

## 4. Week-by-Week Breakdown

### Week 7 — Schema, FTS5 & Keyword-Only Memory

**Goal**: The agent can explicitly store and recall text facts using keyword search. No vectors yet.

| Day | Task |
|---|---|
| Mon | Write migration `002_hierarchical_memory.sql`. Tables: `semantic_memories`, FTS5 virtual table `semantic_memories_fts`, `memory_consolidation_jobs`, `memory_tags`. Add triggers for INSERT/UPDATE/DELETE sync. Run `cargo sqlx migrate run`. |
| Tue | Create `crates/hydragent-memory/src/models.rs`: `SemanticMemory`, `MemoryConsolidationJob` structs with `#[derive(FromRow, Serialize, Deserialize)]`. Write `semantic_store.rs`: async `insert_memory()`, `get_memory()`, `delete_memory()`, `list_memories()`. |
| Wed | Update `messages` table via `ALTER TABLE`: add `chunk_id TEXT` and `requires_consolidation BOOLEAN DEFAULT 1` columns. Update `session_store.rs` to populate `requires_consolidation` on every `append_message()`. |
| Thu | Implement `MemoryStoreTool` in `crates/hydragent-tools/src/memory_store.rs`. On `execute()`: parse params JSON, call `semantic_store::insert_memory()`. Verify FTS5 trigger fires by querying `semantic_memories_fts` after insert. |
| Fri | Implement `MemorySearchTool` in `crates/hydragent-tools/src/memory_search.rs`. For now: pure FTS5 search via `MATCH ?`. Returns top 5 results as formatted JSON. Register both tools in `main.rs`. |
| Sat | Integration test: start agent → "Remember: my Rust project is called Hydragent" → kill → restart → "What is my main project?" → verify agent answers "Hydragent" via `memory_search`. |
| Sun | Implement `MemoryForgetTool`. Wire tier `requires_approval` (Phase 3 gate is a no-op stub for now). Implement `./hydragent memory list` and `./hydragent memory clear` CLI subcommands. |

**Deliverable**: Agent can explicitly store, search (keyword), and forget facts via ReAct tools. Cross-session recall demonstrated manually.

---

### Week 8 — Local Vector Embeddings

**Goal**: Text is converted to 384-dimensional vectors. Cosine similarity search operational.

| Day | Task |
|---|---|
| Mon | Create `crates/hydragent-embed` crate. Add `candle-core`, `candle-transformers`, `tokenizers`, `hf-hub` to its `Cargo.toml`. Ensure pure CPU build works on Windows, macOS, and Linux. Write build test: `cargo build -p hydragent-embed` on all three platforms in CI. |
| Tue | Implement `model_downloader.rs`: `ensure_model_downloaded(data_dir: &str) -> Result<ModelPaths>`. Checks for `data/models/all-MiniLM-L6-v2.safetensors` and `tokenizer.json`; if missing, downloads via `reqwest` with progress bar to stderr. Model URL from `EMBEDDING_MODEL_URL` env var. |
| Wed | Implement `embedder.rs`: `LocalEmbedder::new(model_path, tokenizer_path)` loads BERT model from Safetensors. Implement `embed_text(&self, text: &str) -> Result<Vec<f32>>`: tokenize → tensor → forward pass → mean pooling → L2 normalize. |
| Thu | Implement `pooling.rs`: `mean_pooling(tensor, attention_mask)` function — zero out padding tokens, sum, divide. `l2_normalize(tensor)` — squared sum, sqrt, clamp, divide. Write unit tests with fixed inputs and expected output shapes. |
| Fri | Implement `VectorStore` in `hydragent-memory/src/vector_index.rs`: HNSW wrapper with `insert(id, vec)`, `search(query_vec, k) -> Vec<(String, f32)>`. Implement `save_to_disk()` and `load_from_disk()` via `bincode`. |
| Sat | Wire `LocalEmbedder` into `MemoryStoreTool`: on insert, call `embedder.embed_text()` and `vector_store.insert()` after the SQLite write. Graceful startup: if `data/vectors.bin` exists, reload HNSW on startup. |
| Sun | Embedding quality test: embed 20 known pairs (semantically similar + semantically unrelated). Assert all similar pairs have cosine_similarity > 0.7; unrelated pairs < 0.4. |

**Deliverable**: `cargo test -p hydragent-embed` green. `MemoryStoreTool` now writes to both SQLite AND the HNSW index simultaneously.

---

### Week 9 — Hybrid Search, RRF & Silent Context Injection

**Goal**: Every user message silently triggers memory retrieval. Relevant facts are prepended to the LLM system prompt without the user doing anything.

| Day | Task |
|---|---|
| Mon | Implement `retrieval.rs`: `hybrid_search(query, limit, db, embedder, vector_store) -> Vec<MemoryDocument>`. Runs FTS5 and HNSW searches in parallel via `tokio::join!`. Collects results from both into ranked lists. |
| Tue | Implement the RRF algorithm in `retrieval.rs`. Gather all unique IDs from both result lists. For each ID, compute `rrf_score = Σ(1 / (60 + rank))` across all methods it appeared in. Sort descending by `rrf_score`. Truncate to `limit`. |
| Wed | Hydrate `MemoryDocument` structs: for each top-K RRF ID, `SELECT content, timestamp FROM semantic_memories WHERE id = ?`. Fire-and-forget async update of `access_count` and `last_accessed`. |
| Thu | Implement `context_injector.rs`: `build_system_prompt_with_memory(base, memories, max_tokens) -> String`. Uses `tiktoken-rs::cl100k_base()` for token counting. Iterates memories in RRF-score order, appending until token budget exhausted. |
| Fri | Wire retrieval into `orchestrator.rs`: before dispatching the LLM call, call `hybrid_search(user_message, 10, ...)` and then `build_system_prompt_with_memory(...)`. The LLM now receives a richer system prompt automatically. |
| Sat | `MemorySearchTool` upgrade: replace pure FTS5 with `hybrid_search()`. Return formatted JSON including `rrf_score` and `timestamp` for each result. |
| Sun | Benchmark day: run `cargo bench -- retrieval`. Verify `hybrid_search` with 10k synthetic memories stays under 50 ms. Profile with `tracing::instrument` spans: `fts5_search`, `embed_query`, `hnsw_search`, `rrf_merge`, `hydrate`. |

**Deliverable**: Agent automatically knows context from past sessions. `hybrid_search` benchmark green at < 50 ms.

---

### Week 10 — Dreaming Pipeline & Standing Orders

**Goal**: Background worker compacts raw logs. Standing Orders inject persistent rules. Phase 2 complete.

| Day | Task |
|---|---|
| Mon | Draft the LLM extraction prompt (Section 5.7). Iterate in OpenRouter playground to achieve 100% JSON output compliance across Claude Sonnet, GPT-4o-mini, and Llama-3-8B. |
| Tue | Implement `dream.rs`: `run_dream_cycle(db, llm, embedder, vector_store)`. Fetch batch of 100 unconsolidated messages. Format as `{role}: {content}` log string. Build extraction prompt. Call `llm.generate()`. |
| Wed | Parse JSON extraction result. For each fact with `importance >= 3`, call `semantic_store::insert_memory()` and `vector_store.insert()`. For malformed JSON, log error but don't panic. Mark source messages `requires_consolidation = 0`. |
| Thu | Implement dreaming lifecycle: spawn background `tokio::task` in `main.rs`. Use `tokio::time::interval()` for periodic waking. Implement `memory_consolidation_jobs` tracking — if a job is `processing` on startup (crash recovery), mark it `failed` and retry. |
| Fri | Implement `standing_orders.rs`: `load_standing_orders(config_dir: &str) -> Option<String>`. Reads `config/standing_orders.md`; returns raw Markdown string. In `orchestrator.rs`, if `standing_orders` is `Some(content)`, prepend it to every system prompt before `build_system_prompt_with_memory()`. |
| Sat | End-to-end Dreaming integration test: seed 50 synthetic messages into a test session. Trigger `run_dream_cycle()` manually. Assert `semantic_memories` table has > 3 new rows. Assert all source messages have `requires_consolidation = 0`. |
| Sun | Phase 2 QA. Run `cargo test --workspace` + `pytest adapters/`. Fix all regressions. Update `ARCHITECTURE.md`. Tag `v0.2.0-pre`. Write CHANGELOG entry. |

**Deliverable**: `v0.2.0` tag. All Phase 1 tests still pass. All Phase 2 exit criteria verified.

---

## 5. Component Specifications

### 5.1 Memory Architecture & Tier Design

Hydragent's memory is divided into three distinct tiers, modeled after human cognitive psychology:

```
┌──────────────────────────────────────────────────────────────────────┐
│  TIER 1: WORKING MEMORY — LLM Context Window                        │
│                                                                      │
│  Storage: In-memory Vec<Message>   Capacity: ~128k tokens            │
│  Lifespan: Current conversation    Latency: 0 ms (already loaded)   │
│  Contents: Recent N turns + injected Tier 3 facts                   │
└──────────────────────────┬───────────────────────────────────────────┘
                           │ Filled by Context Injector at turn start
┌──────────────────────────▼───────────────────────────────────────────┐
│  TIER 3: SEMANTIC MEMORY — Knowledge Graph                          │
│                                                                      │
│  Storage: SQLite FTS5 + HNSW index  Capacity: Unlimited             │
│  Lifespan: Indefinite               Latency: 20–50 ms (search)      │
│  Contents: Extracted facts, preferences, project state              │
└──────────────────────────▲───────────────────────────────────────────┘
                           │ Populated by Dreaming Worker
┌──────────────────────────┴───────────────────────────────────────────┐
│  TIER 2: EPISODIC MEMORY — Raw Event Log                            │
│                                                                      │
│  Storage: SQLite messages table    Capacity: Unlimited              │
│  Lifespan: Indefinite              Latency: 2–10 ms (SQL query)     │
│  Contents: Verbatim chat messages, tool calls, raw agent output     │
└──────────────────────────────────────────────────────────────────────┘
```

**Data flow on each user turn**:
1. User message arrives at orchestrator
2. Orchestrator calls `hybrid_search(user_message, limit=10)`
3. Top-ranked memories are passed to `build_system_prompt_with_memory()`
4. Enriched system prompt + recent N turns (Tier 1) → LLM
5. LLM response appended to Tier 2 (SQLite, `requires_consolidation = 1`)

**Data flow during Dreaming** (background):
1. Worker selects 100 messages with `requires_consolidation = 1`
2. Formats as structured log
3. Calls extraction LLM → receives JSON facts
4. Each fact → inserted into `semantic_memories` (triggers FTS5 sync) + HNSW
5. Source messages → `requires_consolidation = 0`

---

### 5.2 Database Schema & Migrations

```sql
-- migrations/002_hierarchical_memory.sql

-- 1. Extend episodic memory (messages table) for consolidation tracking
ALTER TABLE messages ADD COLUMN chunk_id TEXT;
ALTER TABLE messages ADD COLUMN requires_consolidation BOOLEAN NOT NULL DEFAULT 1;

CREATE INDEX IF NOT EXISTS idx_messages_consolidation
    ON messages(requires_consolidation, timestamp);

-- 2. Semantic Memory — the knowledge graph
CREATE TABLE IF NOT EXISTS semantic_memories (
    id               TEXT    PRIMARY KEY,   -- UUID v4
    content          TEXT    NOT NULL,      -- Extracted fact/preference/state in plain English
    source_session_id TEXT,                 -- Session where this was learned (NULL if manually stored)
    timestamp        INTEGER NOT NULL,      -- Unix ms — when this was extracted/stored
    importance_score REAL    NOT NULL DEFAULT 0.5,  -- 0.0–1.0 from LLM importance_1_to_10 / 10
    last_accessed    INTEGER NOT NULL,      -- Unix ms — updated on every retrieval hit
    access_count     INTEGER NOT NULL DEFAULT 0     -- Total number of retrieval hits
);

-- For LRU eviction: sort by last_accessed ASC to find coldest memories
CREATE INDEX IF NOT EXISTS idx_semantic_memories_lru
    ON semantic_memories(last_accessed ASC, importance_score ASC);

-- For importance-based retrieval without FTS/vector
CREATE INDEX IF NOT EXISTS idx_semantic_memories_importance
    ON semantic_memories(importance_score DESC, timestamp DESC);

-- 3. FTS5 Virtual Table for BM25 keyword retrieval
-- ⚠️ Reality: the actual code uses a STANDARD (non-external-content) FTS5
-- table with the default unicode61 tokenizer, NOT trigram. The doc's
-- trigram choice was an aspirational design that wasn't implemented.
CREATE VIRTUAL TABLE IF NOT EXISTS semantic_memories_fts
USING fts5(
    id UNINDEXED,
    content
);

-- 4. Synchronization triggers — present in code with names
-- `fts_insert` / `fts_update` / `fts_delete` (not the `sm_ai` / `sm_au`
-- / `sm_ad` names shown below). Verified by Phase 2 test G1.
CREATE TRIGGER IF NOT EXISTS fts_insert AFTER INSERT ON semantic_memories BEGIN
    INSERT INTO semantic_memories_fts (id, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER IF NOT EXISTS fts_update AFTER UPDATE ON semantic_memories BEGIN
    UPDATE semantic_memories_fts SET content = new.content WHERE id = new.id;
END;

CREATE TRIGGER IF NOT EXISTS fts_delete AFTER DELETE ON semantic_memories BEGIN
    DELETE FROM semantic_memories_fts WHERE id = old.id;
END;

-- 5. Dreaming job tracker — crash recovery support
CREATE TABLE IF NOT EXISTS memory_consolidation_jobs (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          TEXT    NOT NULL,
    start_msg_timestamp INTEGER NOT NULL,
    end_msg_timestamp   INTEGER NOT NULL,
    status              TEXT    NOT NULL
        CHECK (status IN ('pending', 'processing', 'completed', 'failed')),
    tokens_processed    INTEGER DEFAULT 0,
    facts_extracted     INTEGER DEFAULT 0,
    created_at          INTEGER NOT NULL DEFAULT (unixepoch('now','subsec') * 1000),
    completed_at        INTEGER
);

-- 6. Memory tags (many-to-many) — for categorical retrieval in Phase 5+
CREATE TABLE IF NOT EXISTS memory_tags (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id  TEXT    NOT NULL,
    tag        TEXT    NOT NULL,
    FOREIGN KEY(memory_id) REFERENCES semantic_memories(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_memory_tags_by_tag ON memory_tags(tag);
CREATE INDEX IF NOT EXISTS idx_memory_tags_by_memory ON memory_tags(memory_id);
```

---

### 5.3 Local Vector Embedding Pipeline

```rust
// crates/hydragent-embed/src/embedder.rs

use anyhow::{Context, Result};
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::Tokenizer;
use crate::pooling::{mean_pooling, l2_normalize};

/// Thread-safe local embedding model.
/// Wraps a BERT-family model running on CPU via `candle-core`.
/// Clone is cheap — all inner state is Arc'd by candle.
pub struct LocalEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    dimensions: usize,
}

impl LocalEmbedder {
    /// Load the model from local Safetensors files.
    /// This is a blocking operation (~500ms for 90MB model on SSD). Call from `spawn_blocking`.
    pub fn new(model_path: &str, tokenizer_path: &str, config_path: &str) -> Result<Self> {
        // Phase 2: CPU only. Phase 3+ adds --features cuda / --features metal
        let device = Device::Cpu;

        let config_str = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read model config: {}", config_path))?;
        let config: Config = serde_json::from_str(&config_str)
            .context("Failed to parse BERT model config JSON")?;

        let dimensions = config.hidden_size;

        let vb = VarBuilder::from_safetensors(
            vec![model_path],
            candle_core::DType::F32,
            &device,
        ).with_context(|| format!("Failed to load Safetensors from: {}", model_path))?;

        let model = BertModel::load(vb, &config)
            .context("Failed to instantiate BertModel")?;

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        tracing::info!(
            dimensions,
            model_path,
            "LocalEmbedder initialized successfully"
        );

        Ok(Self { model, tokenizer, device, dimensions })
    }

    /// Embed a single text string into a fixed-size vector.
    /// Returns a Vec<f32> of length `self.dimensions` (typically 384).
    ///
    /// Performance: ~20–30ms per call on a modern CPU for sentences up to 128 tokens.
    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        if text.trim().is_empty() {
            anyhow::bail!("Cannot embed empty text");
        }

        // 1. Tokenize with special tokens ([CLS], [SEP])
        let encoding = self.tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed for input '{}...': {}", &text[..text.len().min(50)], e))?;

        let token_ids = encoding.get_ids().to_vec();
        let seq_len = token_ids.len();

        if seq_len == 0 {
            anyhow::bail!("Tokenization resulted in zero tokens for input: {}", text);
        }

        // Clip to 512 tokens (BERT's maximum positional embedding limit)
        let token_ids = if seq_len > 512 {
            tracing::warn!(seq_len, "Input text truncated to 512 tokens");
            token_ids[..512].to_vec()
        } else {
            token_ids
        };

        // 2. Build input tensor: shape [1, seq_len] (batch_size=1)
        let input_ids = Tensor::new(token_ids.as_slice(), &self.device)?.unsqueeze(0)?;

        // 3. Forward pass through BERT: shape [1, seq_len, hidden_size]
        let hidden_states = self.model.forward(&input_ids)?;

        // 4. Mean pooling with attention mask: shape [1, 1, hidden_size]
        let attention_mask = encoding.get_attention_mask();
        let pooled = mean_pooling(&self.device, hidden_states, attention_mask)?;

        // 5. L2 normalization for cosine similarity compatibility: shape [1, 1, hidden_size]
        let normalized = l2_normalize(pooled)?;

        // 6. Flatten to Vec<f32>: length = hidden_size (384 for MiniLM)
        let embedding: Vec<f32> = normalized.squeeze(0)?.squeeze(0)?.to_vec1()?;

        debug_assert_eq!(
            embedding.len(),
            self.dimensions,
            "Embedding dimension mismatch: expected {}, got {}",
            self.dimensions,
            embedding.len()
        );

        Ok(embedding)
    }

    pub fn dimensions(&self) -> usize { self.dimensions }
}

// crates/hydragent-embed/src/pooling.rs

use anyhow::Result;
use candle_core::{Device, Tensor};

/// Attention-mask-aware mean pooling.
/// Padding tokens (mask=0) are zeroed out before averaging.
pub fn mean_pooling(
    device: &Device,
    hidden_states: Tensor,   // [batch=1, seq_len, hidden_size]
    attention_mask: &[u32],  // [seq_len] — 1 for real tokens, 0 for padding
) -> Result<Tensor> {
    // Build mask tensor: [1, seq_len, 1] for broadcasting
    let mask = Tensor::new(attention_mask, device)?
        .unsqueeze(0)?   // [1, seq_len]
        .unsqueeze(2)?   // [1, seq_len, 1]
        .to_dtype(candle_core::DType::F32)?;

    // Zero out padding positions
    let masked = hidden_states.broadcast_mul(&mask)?;

    // Sum over sequence dimension
    let sum = masked.sum_keepdim(1)?;               // [1, 1, hidden_size]

    // Count non-padding tokens (at least 1 to avoid div/0)
    let count = mask.sum_keepdim(1)?.clamp_min(1e-9)?;  // [1, 1, 1]

    // Divide to get mean
    Ok(sum.broadcast_div(&count)?)
}

/// L2-normalize the embedding vector to unit sphere.
/// Required for cosine similarity: cos(a, b) = dot(normalize(a), normalize(b))
pub fn l2_normalize(tensor: Tensor) -> Result<Tensor> {
    let norm = tensor.sqr()?.sum_keepdim(2)?.sqrt()?.clamp_min(1e-9)?;
    Ok(tensor.broadcast_div(&norm)?)
}
```

---

### 5.4 Vector Index — **Linear Scan** (NOT HNSW)

> ⚠️ **Doc vs Code Reality**: The original spec called for `hnsw_rs` with
> `DistCosine`. The actual implementation is a simple linear scan over
> a `HashMap<String, Vec<f32>>`. Real HNSW integration is deferred to a
> later phase (see `doc/archive/phases/PHASE_2_FINAL_REPORT.md` divergence table). The 100k-fact /
> 10ms latency target is **not** met by the current code.

```rust
// crates/hydragent-memory/src/vector_index.rs

use serde::{Serialize, Deserialize};
use std::path::Path;
use anyhow::Result;
use std::collections::HashMap;

/// In-memory vector store. Linear scan over HashMap.
///
/// ⚠️ Performance: O(N) per query. Not HNSW. Do not use beyond ~10k facts
/// without a real index migration. Phase 2 stress test passed because
/// the test corpus is small (≤100 facts per run).
#[derive(Serialize, Deserialize, Default)]
pub struct VectorStore {
    embeddings: HashMap<String, Vec<f32>>,
}

impl VectorStore {
    pub fn new() -> Self {
        Self { embeddings: HashMap::new() }
    }

    pub fn insert(&mut self, id: String, vector: Vec<f32>) {
        self.embeddings.insert(id, vector);
    }

    /// Cosine similarity search. Iterates ALL stored vectors.
    /// Returns top-k by descending similarity.
    pub fn search(&self, query_vec: &[f32], k: usize) -> Vec<(String, f32)> {
        let mut results = Vec::new();
        for (id, vec) in &self.embeddings {
            let sim = hydragent_embed::cosine_similarity(query_vec, vec);
            results.push((id.clone(), sim));
        }
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);
        results
    }

    pub fn delete(&mut self, id: &str) {
        self.embeddings.remove(id);
    }

    pub fn clear(&mut self) {
        self.embeddings.clear();
    }

    /// Persist to disk via bincode. The on-disk format is just a serialized
    /// HashMap; no index structure.
    pub fn save_to_disk(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = bincode::serialize(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn load_from_disk(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let store: Self = bincode::deserialize(&bytes)?;
        Ok(store)
    }
}
```

> **Migration plan (Phase 5+)**: replace this struct with
> `hnsw_rs::Hnsw<'static, f32, DistCosine>` (or `instant-distance`) and
> add a real benchmark in `benches/retrieval_benchmark.rs`. Until then,
> the search cost is O(N) per query and grows linearly with fact count.

---

### 5.5 Hybrid Retrieval & Reciprocal Rank Fusion (RRF)

```rust
// crates/hydragent-memory/src/retrieval.rs

use std::collections::{HashMap, HashSet};
use sqlx::SqlitePool;
use hydragent_types::MemoryDocument;

/// Performs hybrid BM25 + vector search and merges results with Reciprocal Rank Fusion.
///
/// # Algorithm
/// 1. Execute FTS5 BM25 keyword search → ranked list A
/// 2. Execute HNSW cosine similarity search → ranked list B
/// 3. For each document in (A ∪ B), compute:
///    `rrf_score = 1/(60 + rank_in_A) + 1/(60 + rank_in_B)`
///    where rank is omitted if the document didn't appear in that list
/// 4. Sort by rrf_score descending; return top `limit`
///
/// # References
/// Cormack, Clarke & Buettcher (2009). "Reciprocal rank fusion outperforms condorcet and individual rank learning methods."
#[tracing::instrument(skip(db, embedder, vector_store))]
pub async fn hybrid_search(
    query: &str,
    limit: usize,
    db: &SqlitePool,
    embedder: &hydragent_embed::LocalEmbedder,
    vector_store: &VectorStore,
) -> anyhow::Result<Vec<MemoryDocument>> {

    // === PARALLEL EXECUTION ===
    // Run FTS5 and embedding generation concurrently.
    // FTS5 is async (sqlx); embedding is CPU-bound (moved to spawn_blocking).
    let fts_future = run_fts5_search(db, query, 20);
    let embed_future = {
        let embedder = embedder.clone();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || embedder.embed_text(&query))
    };

    let (fts_results, embed_result) = tokio::join!(fts_future, embed_future);

    let keyword_rows = fts_results?;
    let query_vec = embed_result??;
    let vector_hits = vector_store.search(&query_vec, 20)?;

    // === BUILD RANK MAPS ===
    let keyword_ranks: HashMap<String, usize> = keyword_rows
        .iter()
        .enumerate()
        .map(|(i, row)| (row.id.clone(), i + 1))  // 1-indexed
        .collect();

    let vector_ranks: HashMap<String, usize> = vector_hits
        .iter()
        .enumerate()
        .map(|(i, (uuid, _))| (uuid.clone(), i + 1))  // 1-indexed
        .collect();

    // === RRF SCORING ===
    const RRF_K: f64 = 60.0;

    let all_ids: HashSet<String> = keyword_ranks.keys()
        .chain(vector_ranks.keys())
        .cloned()
        .collect();

    let mut rrf_scores: Vec<(String, f64)> = all_ids.into_iter().map(|id| {
        let mut score = 0.0_f64;
        if let Some(&rank) = keyword_ranks.get(&id) {
            score += 1.0 / (RRF_K + rank as f64);
        }
        if let Some(&rank) = vector_ranks.get(&id) {
            score += 1.0 / (RRF_K + rank as f64);
        }
        (id, score)
    }).collect();

    rrf_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    rrf_scores.truncate(limit);

    // === HYDRATE FROM SQLite ===
    let mut documents = Vec::with_capacity(rrf_scores.len());
    for (id, rrf_score) in rrf_scores {
        let row = sqlx::query!(
            "SELECT content, timestamp, importance_score FROM semantic_memories WHERE id = ?",
            id
        )
        .fetch_one(db)
        .await?;

        // Fire-and-forget access tracking (non-blocking)
        {
            let db = db.clone();
            let id = id.clone();
            tokio::spawn(async move {
                let _ = sqlx::query!(
                    "UPDATE semantic_memories SET access_count = access_count + 1, \
                     last_accessed = (unixepoch('now','subsec') * 1000) WHERE id = ?",
                    id
                )
                .execute(&db)
                .await;
            });
        }

        documents.push(MemoryDocument {
            id,
            content: row.content,
            timestamp: row.timestamp,
            importance_score: row.importance_score,
            rrf_score,
        });
    }

    tracing::debug!(
        query,
        results = documents.len(),
        keyword_hits = keyword_ranks.len(),
        vector_hits = vector_ranks.len(),
        "hybrid_search complete"
    );

    Ok(documents)
}

async fn run_fts5_search(
    db: &SqlitePool,
    query: &str,
    limit: i64,
) -> anyhow::Result<Vec<FtsRow>> {
    // SQLite FTS5: rows ordered by rank (most negative = best match)
    let rows = sqlx::query_as!(
        FtsRow,
        r#"
        SELECT rowid as "id: String", content
        FROM semantic_memories_fts
        WHERE semantic_memories_fts MATCH ?
        ORDER BY rank
        LIMIT ?
        "#,
        query,
        limit
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

#[derive(sqlx::FromRow)]
struct FtsRow { id: String, content: String }
```

---

### 5.6 Context Injection & Token Budget Manager

```rust
// crates/hydragent-memory/src/context_injector.rs

use hydragent_types::MemoryDocument;
use tiktoken_rs::cl100k_base;

/// Prepend retrieved memories to the base system prompt,
/// respecting a hard token ceiling.
///
/// Memories are injected in descending RRF score order.
/// Once the token ceiling is reached, remaining memories are silently dropped.
pub fn build_system_prompt_with_memory(
    base_prompt: &str,
    memories: &[MemoryDocument],
    max_memory_tokens: usize,
) -> String {
    if memories.is_empty() {
        return base_prompt.to_string();
    }

    let bpe = cl100k_base().expect("tiktoken cl100k_base failed to load");
    let base_token_count = bpe.encode_with_special_tokens(base_prompt).len();

    let header = concat!(
        "\n\n---\n",
        "# Retrieved Long-term Memory\n",
        "The following facts were retrieved from your persistent knowledge graph. ",
        "They are ranked by relevance to the current conversation. ",
        "Prioritize recent user input if it contradicts these facts.\n\n"
    );

    let header_tokens = bpe.encode_ordinary(header).len();
    let mut used_tokens = base_token_count + header_tokens;
    let mut memory_lines = Vec::new();
    let mut truncated = 0usize;

    for doc in memories {
        let ts = chrono::DateTime::from_timestamp_millis(doc.timestamp)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "unknown date".to_string());

        let line = format!(
            "- [{}] (score={:.3}) {}\n",
            ts, doc.rrf_score, doc.content
        );

        let line_tokens = bpe.encode_ordinary(&line).len();

        if used_tokens + line_tokens > base_token_count + max_memory_tokens {
            truncated += 1;
            continue;
        }

        memory_lines.push(line);
        used_tokens += line_tokens;
    }

    if truncated > 0 {
        tracing::debug!(
            truncated,
            max_memory_tokens,
            "Memory context truncated due to token budget"
        );
    }

    if memory_lines.is_empty() {
        return base_prompt.to_string();
    }

    format!("{}{}{}", base_prompt, header, memory_lines.join(""))
}
```

---

### 5.7 Dreaming Pipeline (Memory Consolidation)

#### 5.7.1 LLM Extraction Prompt

The extraction prompt is the most important design surface in the Dreaming pipeline. It must produce valid JSON 100% of the time, regardless of whether the model is Claude Sonnet, GPT-4o-mini, or a local Llama-3-8B.

```text
You are a knowledge extraction pipeline. Analyze the conversation log below and
extract long-term, reusable facts that an AI assistant should remember indefinitely.

EXTRACTION RULES:
1. Extract ONLY explicit, factual statements. Do NOT infer or guess.
2. Ignore: pleasantries, greetings, questions without answers, tool execution logs,
   error messages, and temporary debugging steps.
3. Include: user preferences, project facts, environmental info, key decisions,
   personal data the user volunteered, and explicit "remember this" requests.
4. Write each fact as a single, complete, self-contained sentence.
5. Assign importance_1_to_10 based on how likely this fact is to be useful weeks from now.
   - 9–10: Identity, critical credentials scope names, project architecture
   - 7–8: Preferences, tech stack choices, team members
   - 5–6: Configuration details, dependency versions
   - 3–4: Incidental facts that might matter later
   - 1–2: Noise — skip these

OUTPUT FORMAT: Valid JSON only. No markdown, no backticks, no explanation.

{
  "extracted_facts": [
    {
      "fact": "User's primary programming language is Rust.",
      "category": "technical",
      "importance_1_to_10": 8
    }
  ]
}

ALLOWED CATEGORIES: personal | technical | preference | project_state

CONVERSATION LOG:
{CONVERSATION_LOG}
```

#### 5.7.2 Dream Cycle Implementation

```rust
// crates/hydragent-core/src/dream.rs

use serde::Deserialize;
use sqlx::SqlitePool;
use hydragent_embed::LocalEmbedder;
use hydragent_memory::VectorStore;
use hydragent_model::ModelProvider;

#[derive(Debug, Deserialize)]
struct ExtractionResponse {
    extracted_facts: Vec<ExtractedFact>,
}

#[derive(Debug, Deserialize)]
struct ExtractedFact {
    fact: String,
    category: String,
    importance_1_to_10: u8,
}

/// The minimum importance score to store a fact. Facts below this threshold are discarded.
const MIN_IMPORTANCE: u8 = 3;

/// Maximum messages to process per dream cycle. Limits LLM prompt size.
const BATCH_SIZE: i64 = 100;

/// Entry point for the background dreaming worker.
/// Should be called from `tokio::spawn` in `main.rs`.
pub async fn run_dream_cycle(
    db: &SqlitePool,
    llm: &dyn ModelProvider,
    embedder: &LocalEmbedder,
    vector_store: &VectorStore,
) -> anyhow::Result<DreamStats> {
    let mut stats = DreamStats::default();

    // 1. Fetch a batch of unconsolidated messages
    let rows = sqlx::query!(
        r#"
        SELECT id, role, content, session_id
        FROM messages
        WHERE requires_consolidation = 1
        ORDER BY timestamp ASC
        LIMIT ?
        "#,
        BATCH_SIZE
    )
    .fetch_all(db)
    .await?;

    if rows.is_empty() {
        tracing::debug!("Dream cycle: no unconsolidated messages, going back to sleep.");
        return Ok(stats);
    }

    tracing::info!(batch_size = rows.len(), "Dream cycle: processing message batch");

    // 2. Format as conversation log
    let mut log_lines = Vec::new();
    let mut row_ids: Vec<String> = Vec::new();

    for row in &rows {
        log_lines.push(format!("{}: {}", row.role, row.content));
        row_ids.push(row.id.clone());
        stats.messages_processed += 1;
    }
    let log_text = log_lines.join("\n\n");

    // 3. Build and submit extraction prompt
    let prompt = build_extraction_prompt(&log_text);
    let raw_json = match llm.generate_non_streaming(&prompt).await {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(error = %e, "Dream cycle: LLM call failed");
            return Ok(stats);
        }
    };

    // 4. Parse JSON (gracefully handle malformed output)
    let extraction: ExtractionResponse = match serde_json::from_str(&raw_json) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!(
                error = %e,
                raw_output_len = raw_json.len(),
                "Dream cycle: failed to parse extraction JSON — skipping batch"
            );
            // Still mark as consolidated so we don't loop forever on unparseable batches
            mark_consolidated(db, &row_ids).await?;
            return Ok(stats);
        }
    };

    // 5. Store extracted facts
    for fact in extraction.extracted_facts {
        if fact.importance_1_to_10 < MIN_IMPORTANCE {
            stats.facts_skipped += 1;
            continue;
        }

        let memory_id = uuid::Uuid::new_v4().to_string();
        let importance = fact.importance_1_to_10 as f64 / 10.0;
        let now = chrono::Utc::now().timestamp_millis();

        // 5a. Insert into SQLite (triggers FTS5 sync automatically)
        sqlx::query!(
            r#"
            INSERT INTO semantic_memories
                (id, content, timestamp, importance_score, last_accessed)
            VALUES (?, ?, ?, ?, ?)
            "#,
            memory_id, fact.fact, now, importance, now
        )
        .execute(db)
        .await?;

        // 5b. Generate and store embedding
        let embedder = embedder.clone();
        let fact_text = fact.fact.clone();
        let embed_result = tokio::task::spawn_blocking(move || {
            embedder.embed_text(&fact_text)
        }).await??;

        vector_store.insert(memory_id, embed_result)?;
        stats.facts_stored += 1;
    }

    // 6. Mark source messages as consolidated
    mark_consolidated(db, &row_ids).await?;

    tracing::info!(
        ?stats,
        "Dream cycle: batch complete"
    );

    Ok(stats)
}

async fn mark_consolidated(db: &SqlitePool, ids: &[String]) -> anyhow::Result<()> {
    for id in ids {
        sqlx::query!(
            "UPDATE messages SET requires_consolidation = 0 WHERE id = ?",
            id
        )
        .execute(db)
        .await?;
    }
    Ok(())
}

fn build_extraction_prompt(log_text: &str) -> String {
    include_str!("../prompts/extraction_prompt.txt")
        .replace("{CONVERSATION_LOG}", log_text)
}

#[derive(Debug, Default)]
pub struct DreamStats {
    pub messages_processed: usize,
    pub facts_stored: usize,
    pub facts_skipped: usize,
}
```

#### 5.7.3 Background Worker Lifecycle

```rust
// In crates/hydragent-core/src/main.rs

if config.enable_dreaming {
    let db = db_pool.clone();
    let llm = llm_provider.clone();
    let embedder = embedder.clone();
    let vec = vector_store.clone();
    let interval_secs = config.dreaming_interval_sec;

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(
            std::time::Duration::from_secs(interval_secs)
        );
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            tracing::info!("Dreaming worker waking up...");

            match run_dream_cycle(&db, &*llm, &embedder, &vec).await {
                Ok(stats) => tracing::info!(?stats, "Dream cycle completed"),
                Err(e) => tracing::error!(error = %e, "Dream cycle error"),
            }
        }
    });
}
```

---

### 5.8 Memory Tools

#### `MemoryStoreTool`

```rust
// crates/hydragent-tools/src/memory_store.rs

use async_trait::async_trait;
use serde::Deserialize;
use hydragent_types::{ToolResult, ToolStatus};

pub struct MemoryStoreTool {
    db: sqlx::SqlitePool,
    embedder: Arc<hydragent_embed::LocalEmbedder>,
    vector_store: Arc<hydragent_memory::VectorStore>,
}

#[derive(Deserialize)]
struct MemoryStoreParams {
    content: String,
    tags: Vec<String>,
}

#[async_trait]
impl Tool for MemoryStoreTool {
    fn name(&self) -> &str { "memory_store" }

    fn description(&self) -> &str {
        "Store a critical fact, user preference, or project state into long-term memory. \
         Use when the user says 'remember this', makes a key technical decision, \
         or reveals a fact that should persist across sessions."
    }

    fn params_schema(&self) -> &str {
        r#"{
          "type": "object",
          "required": ["content", "tags"],
          "properties": {
            "content": {
              "type": "string",
              "description": "The fact to remember, written as a single, self-contained sentence."
            },
            "tags": {
              "type": "array",
              "items": { "type": "string" },
              "description": "Categories: [\"personal\", \"technical\", \"preference\", \"project_state\"]"
            }
          }
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();

        let params: MemoryStoreParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => return ToolResult::failure(format!("Invalid params: {}", e)),
        };

        let memory_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();

        // Insert into SQLite (triggers FTS5 update automatically)
        if let Err(e) = sqlx::query!(
            "INSERT INTO semantic_memories (id, content, timestamp, importance_score, last_accessed) VALUES (?, ?, ?, ?, ?)",
            memory_id, params.content, now, 0.8_f64, now  // Manually stored = high importance
        ).execute(&self.db).await {
            return ToolResult::failure(format!("DB insert failed: {}", e));
        }

        // Store tags
        for tag in &params.tags {
            let _ = sqlx::query!(
                "INSERT INTO memory_tags (memory_id, tag) VALUES (?, ?)",
                memory_id, tag
            ).execute(&self.db).await;
        }

        // Generate and store embedding
        let content = params.content.clone();
        let embedder = self.embedder.clone();
        let embed_result = tokio::task::spawn_blocking(move || {
            embedder.embed_text(&content)
        }).await;

        if let Ok(Ok(vec)) = embed_result {
            let _ = self.vector_store.insert(memory_id.clone(), vec);
        }

        ToolResult {
            call_id: String::new(),
            output_json: serde_json::json!({
                "stored": true,
                "memory_id": memory_id,
                "content": params.content
            }).to_string(),
            status: ToolStatus::Success,
            execution_ms: start.elapsed().as_millis() as u64,
            error_message: None,
        }
    }
}
```

---

### 5.9 Soul Tool (a.k.a. "Standing Orders")

> ⚠️ **Doc vs Code Reality**: This section was originally titled
> "Standing Orders Module" and described a `load_standing_orders()` function
> reading `config/standing_orders.md`. **The actual implementation** is
> the **`soul` tool** (struct `SoulTool` in
> `crates/hydragent-tools/src/standing_orders.rs`) which reads and writes
> **`config/SOUL.md`**. The file name is a deliberate match for the
> docstring reference; treat `soul` ↔ `standing_orders` as synonyms.

The `soul` tool is a persistent behavioral-rule store that applies to
every session, regardless of user input. It is exposed to the LLM as a
normal `Tool`, so the model can `read` / `add` / `remove` rules
conversationally.

```rust
// crates/hydragent-tools/src/standing_orders.rs  (file name is historical)

use std::path::PathBuf;
use serde::Deserialize;
use hydragent_types::{ToolResult, ToolStatus, Tool};

pub struct SoulTool {
    config_dir: PathBuf,
}

#[derive(Deserialize)]
struct SoulParams {
    action: String,        // "read" | "add" | "remove"
    #[serde(default)]
    rule: Option<String>,
    #[serde(default)]
    rule_id: Option<usize>,
}

impl Tool for SoulTool {
    fn name(&self) -> &str { "soul" }

    fn description(&self) -> &str {
        "Allows viewing, adding, or removing persistent behavioral rules \
         or instructions in `./config/SOUL.md` that guide the AI's behavior."
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        // ... reads / appends / removes numbered rules in SOUL.md ...
        // The file path is `config_dir.join("SOUL.md")` — NOT standing_orders.md.
    }
}
```

**Example `config/SOUL.md`** (auto-created on first `add`):

```markdown
# Agent Soul & Personality
- Name: Hydra
- Tone: Helpful, intelligent, and adaptive.

# Behavior Rules
1. Always use concise, technically precise language.
2. Never reveal API keys, vault contents, or credentials in any response.
3. Never execute destructive commands (rm -rf, DROP TABLE) without explicit confirmation.
```

**Injection point**: `crates/hydragent-core/src/orchestrator.rs` reads
`SOUL.md` on every system-prompt build and prepends the contents to the
LLM's system message. Verified by Phase 2 tests F1 + F2.

---

### 5.10 Memory Lifecycle & LRU Eviction

For very long-running deployments, the `semantic_memories` table could grow unbounded. Phase 2 includes a lightweight LRU eviction stub that is configurable but disabled by default.

```sql
-- Eviction query: delete the N coldest, least important memories
-- Run when COUNT(semantic_memories) > MAX_MEMORY_ROWS
DELETE FROM semantic_memories
WHERE id IN (
    SELECT id FROM semantic_memories
    WHERE importance_score < 0.5   -- Never evict high-importance memories
    ORDER BY last_accessed ASC,    -- Coldest first
             access_count ASC      -- Least-used first
    LIMIT ?                        -- N rows to evict
);
```

---

## 6. Built-in Tools (Phase 2 Additions)

Three new tools are added to the registry in `main.rs`. All three call into the `hydragent-memory` crate.

### `memory_store`

```yaml
name: memory_store
description: >
  Store a critical fact, preference, or decision into long-term memory.
  Use when the user says "remember this", makes an architectural decision,
  or reveals persistent project context.
tier: auto_approve
params_schema:
  type: object
  required: [content, tags]
  properties:
    content:
      type: string
      description: "The fact to store. Must be a single, self-contained, objective sentence."
    tags:
      type: array
      items: { type: string }
      description: "Category tags: personal | technical | preference | project_state"

output:
  type: object
  properties:
    stored: { type: boolean }
    memory_id: { type: string, description: "UUID of the new memory" }
    content: { type: string }
```

---

### `memory_search`

```yaml
name: memory_search
description: >
  Query your long-term knowledge graph using hybrid BM25 + semantic search.
  Use when you suspect relevant context exists from past sessions or conversations.
tier: auto_approve
params_schema:
  type: object
  required: [query]
  properties:
    query:
      type: string
      description: "Natural language search query."
    limit:
      type: integer
      default: 5
      minimum: 1
      maximum: 20
      description: "Maximum number of memories to return."

output:
  type: object
  properties:
    results:
      type: array
      items:
        type: object
        properties:
          memory_id: { type: string }
          content: { type: string }
          rrf_score: { type: number, description: "Relevance score (higher = more relevant)" }
          date: { type: string, description: "When this was learned (YYYY-MM-DD)" }
```

---

### `memory_forget`

```yaml
name: memory_forget
description: >
  Permanently delete a specific memory from the knowledge graph.
  Use when the user says a previous fact is wrong, outdated, or requests data deletion.
  Requires user approval (destructive action).
tier: prompt          # Phase 3 will enforce this; Phase 2 treats as auto_approve stub
params_schema:
  type: object
  required: [memory_id]
  properties:
    memory_id:
      type: string
      description: "UUID of the memory to delete (from memory_search results)."

security:
  - Cascades to memory_tags and semantic_memories_fts via triggers
  - Also removes the corresponding vector from the HNSW index
```

---

## 7. Configuration & Environment

### Updated `.env.example`

```ini
# ── LLM Providers (Phase 1, unchanged) ────────────────────────────────────
OPENROUTER_API_KEYS=sk-or-v1-...
PRIMARY_MODEL=nvidia/nemotron-3-ultra-550b-a55b:free
FALLBACK_MODELS=openai/gpt-4o-mini,meta-llama/llama-3-8b-instruct:free

# ── Phase 2: Semantic Memory ───────────────────────────────────────────────

# Enable automatic context injection before every LLM call
ENABLE_SEMANTIC_MEMORY=true

# Number of memories to retrieve per turn (before token budget truncation)
MEMORY_RETRIEVAL_LIMIT=10

# Maximum tokens dedicated to injected memory in the system prompt
MEMORY_CONTEXT_TOKEN_LIMIT=2000

# ── Phase 2: Embedding Model ───────────────────────────────────────────────

# Directory where the model files are stored/downloaded
EMBEDDING_MODEL_DIR=./data/models

# HuggingFace URL for model download (on first run)
EMBEDDING_MODEL_URL=https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/model.safetensors
EMBEDDING_TOKENIZER_URL=https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json
EMBEDDING_CONFIG_URL=https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/config.json

# Output vector dimensions (must match the model: all-MiniLM-L6-v2 = 384)
EMBEDDING_DIMENSIONS=384

# ── Phase 2: HNSW Vector Index ─────────────────────────────────────────────

# Path to serialized HNSW index file
VECTOR_INDEX_PATH=./data/vectors.bin

# ── Phase 2: Dreaming (Memory Consolidation) ───────────────────────────────

# Enable background memory consolidation worker
ENABLE_DREAMING=true

# Seconds between dream cycles (3600 = 1 hour, 300 = 5 min for dev testing)
DREAMING_INTERVAL_SEC=3600

# Model to use for log extraction (can be cheaper than PRIMARY_MODEL)
DREAMING_MODEL=openai/gpt-4o-mini

# ── Phase 2: Standing Orders ───────────────────────────────────────────────

# Path to persistent behavioral rules injected at every session start
STANDING_ORDERS_PATH=./config/standing_orders.md
```

---

## 8. Testing Strategy

### 8.1 Unit Tests

| Test File | What It Covers |
|---|---|
| `embedder_test.rs` | `embed_text("")` returns `Err`; identical strings produce identical vectors; `embed("dog")` is cosine-closer to `embed("puppy")` than `embed("car")`; output dimension == 384 |
| `pooling_test.rs` | `mean_pooling` with all-ones mask == `mean_pooling` with no padding; `l2_normalize` output has unit norm (∣v∣ = 1.0 ± 1e-6) |
| `vector_index_test.rs` | Insert 100 vectors; search returns top-K in descending similarity; `save_to_disk()` + `load_from_disk()` round-trip preserves id_map count |
| `retrieval_test.rs` | RRF score of document in both lists > score of document in only one list; document at rank 1 always beats document at rank 60; empty queries return empty results |
| `context_injector_test.rs` | Token count of output ≤ `base_tokens + max_memory_tokens`; empty memories return base_prompt unchanged; all memory entries appear in correct timestamp format |
| `dream_test.rs` | Mock LLM returning malformed JSON → zero panics, zero new rows, consolidated = 0 on source messages; valid JSON → `facts_stored` matches extraction count; importance < 3 → skipped |
| `standing_orders_test.rs` | Missing file returns `None`; empty file returns `None`; valid file returns `Some(content)` |
| `fts5_trigger_test.rs` | Insert 10 rows → FTS count = 10; delete 3 → FTS count = 7; update 2 → FTS content updated |

### 8.2 Integration Tests

#### The Amnesia Test

```bash
#!/bin/bash
# tests/integration/memory_amnesia_test.sh
set -e

SESSION_A="amnesia-test-session-a"
SESSION_B="amnesia-test-session-b"

echo "=== Phase 2 Amnesia Test ==="

# 1. Session A: teach the agent a fact
echo "My server's IP is 10.0.0.42" | ./target/debug/hydragent --session "$SESSION_A"

# 2. Wait for dreaming (test config: DREAMING_INTERVAL_SEC=1)
sleep 3

# 3. Kill agent
pkill hydragent 2>/dev/null || true

# 4. Session B: ask for the fact without mentioning it
RESPONSE=$(echo "What is my server IP?" | ./target/debug/hydragent --session "$SESSION_B")

echo "Agent response: $RESPONSE"

if echo "$RESPONSE" | grep -q "10.0.0.42"; then
    echo "✅ PASS: Cross-session memory recall verified"
    exit 0
else
    echo "❌ FAIL: Amnesia — agent did not recall IP address"
    echo "Semantic memories table:"
    sqlite3 ./data/sessions/hydragent_memory.db "SELECT content FROM semantic_memories LIMIT 10;"
    exit 1
fi
```

#### Dreaming Pipeline Test

```rust
// tests/integration/dreaming_test.rs

#[tokio::test]
async fn test_dream_cycle_populates_semantic_memory() {
    let db = setup_test_db().await;

    // Seed 50 synthetic messages
    for i in 0..50 {
        sqlx::query!(
            "INSERT INTO messages (id, session_id, role, content, timestamp) VALUES (?, ?, ?, ?, ?)",
            uuid::Uuid::new_v4().to_string(),
            "test-session",
            if i % 2 == 0 { "user" } else { "assistant" },
            format!("Message {}: My deployment target is AWS us-east-1.", i),
            chrono::Utc::now().timestamp_millis()
        ).execute(&db).await.unwrap();
    }

    let llm = MockLLM::returns(r#"{"extracted_facts": [
        {"fact": "User deploys to AWS us-east-1.", "category": "technical", "importance_1_to_10": 8}
    ]}"#);
    let embedder = create_test_embedder();
    let vector_store = VectorStore::new(":memory:", 384);

    run_dream_cycle(&db, &llm, &embedder, &vector_store).await.unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM semantic_memories")
        .fetch_one(&db).await.unwrap();
    assert!(count >= 1, "Expected at least 1 semantic memory, got {}", count);

    let unconsolidated: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE requires_consolidation = 1"
    ).fetch_one(&db).await.unwrap();
    assert_eq!(unconsolidated, 0, "All messages should be marked consolidated");
}
```

### 8.3 Manual QA Checklist (Phase 2 Sign-off)

```
[ ] Cold-start with fresh data/ directory — embedding model downloads automatically
[ ] Ask agent 5 questions; store 5 facts manually via chat ("remember that...")
[ ] Kill agent; restart with new session ID
[ ] Ask agent "What do you remember about me?" → should surface stored facts
[ ] Ask semantic query ("what programming language do I use?") → RRF retrieves fact
[ ] Set DREAMING_INTERVAL_SEC=5 in .env; have 20-message session; wait 10s
    → Check sqlite3 semantic_memories table; verify new rows extracted
[ ] Set MEMORY_CONTEXT_TOKEN_LIMIT=100; ask long query
    → Verify injected memory block is truncated (check tracing logs)
[ ] Add standing_orders.md; verify its rules appear in system prompt
[ ] cargo test --workspace → exits 0 with zero warnings
[ ] pytest adapters/ → exits 0
```

---

## 9. Performance Targets

| Metric | Target | Measurement Method |
|---|---|---|
| Embedding generation latency | < 30 ms | `Instant::now()` around `embedder.embed_text()` in unit test |
| FTS5 keyword search (100k rows) | < 5 ms | `cargo bench -- fts5_search` |
| HNSW vector search (100k vectors) | < 2 ms | `cargo bench -- hnsw_search` |
| RRF merge (40 candidates) | < 1 ms | Pure in-memory HashMap operations |
| SQLite hydration (10 documents) | < 10 ms | sqlx async query with indexed lookup |
| Full `hybrid_search` end-to-end | < 50 ms | `cargo bench -- hybrid_search` |
| Context injection (10 memories) | < 2 ms | `tiktoken_rs` encode is O(N tokens) |
| `memory_store` tool e2e | < 60 ms | SQLite write + embedding + HNSW insert |
| Dreaming cycle (100 messages) | Non-blocking | Spawned on separate Tokio task; EventBus unaffected |
| Model download (first run) | < 120 s | Depends on connection speed; progress bar shown |
| RAM overhead (loaded model) | < 200 MB | `/proc/{pid}/status VmRSS` after `LocalEmbedder::new()` |

---

## 10. Risks & Mitigations

| Risk | Category | Likelihood | Impact | Mitigation Strategy |
|---|---|---|---|---|
| **`candle-core` compilation failure on Windows** | Build | High | High | Test on Windows CI from Day 1. Ensure `features = []` (CPU only) in workspace `Cargo.toml`. Document in `CONTRIBUTING.md`. |
| **Dreaming hallucinations** (LLM invents false facts) | LLM Quality | Medium | High | Strict extraction prompt: "DO NOT INFER. ONLY EXTRACT EXPLICITLY STATED FACTS." Allow users to run `./hydragent memory list` and `memory_forget` via CLI to correct mistakes. |
| **Context window stuffing** (too many memories injected) | LLM Quality | Medium | High | Hard `tiktoken-rs` cap enforced in `build_system_prompt_with_memory()`. Default `MEMORY_CONTEXT_TOKEN_LIMIT=2000` is conservative. |
| **HNSW index corruption on crash** | Storage | Low | Medium | `load_or_create()` detects corrupt magic bytes and silently regenerates by re-embedding all rows in `semantic_memories`. |
| **Embedding model not downloaded in offline env** | UX | Low | Medium | Check for model files on startup; emit clear error: "Run with network access once to download embedding model (90 MB)". |
| **FTS5 trigram index growing large** | Storage | Low | Low | FTS5 trigram uses ~3× storage of content column. At 100k facts × 100 chars avg = ~30 MB overhead — acceptable for Phase 2. |
| **`sqlx` offline mode issues in CI** | Build | Medium | Low | `cargo sqlx prepare --workspace` commits `sqlx-data.json` for offline/CI builds. Enforce in CI with `SQLX_OFFLINE=true`. |
| **OpenRouter rate limits during dreaming** | Cost | Low | Medium | `DREAMING_MODEL` defaults to `gpt-4o-mini` (cheapest). Exponential backoff on 429. `DREAMING_INTERVAL_SEC` configurable. |

---

## 11. Definition of Done

Phase 2 is **complete** when all of the following are true:

### Code

- [ ] `cargo build --workspace` and `cargo test --workspace` both exit 0 with `RUSTFLAGS="-D warnings"`
- [ ] `cargo clippy --workspace -- -D warnings` exits 0
- [ ] All Python adapter tests pass: `pytest adapters/ -v` exits 0
- [ ] No `TODO` or `FIXME` in Phase 2 source files (deferred items tracked as GitHub issues with `phase-3+` label)
- [ ] Every public Rust item in Phase 2 crates has a `///` doc comment

### Database

- [ ] `cargo sqlx migrate run` on a fresh SQLite file applies both `001_initial.sql` and `002_hierarchical_memory.sql` cleanly
- [ ] `cargo sqlx prepare --workspace` generates `sqlx-data.json` for offline CI builds
- [ ] FTS5 trigger unit test: insert/update/delete 100 rows → FTS table stays in sync

### Functional

- [ ] Agent passes the Amnesia Integration Test (cross-session recall without explicit `memory_search` call)
- [ ] Dreaming integration test: 50 synthetic messages → ≥ 3 semantic memories extracted
- [ ] `hybrid_search` returns different rankings than pure FTS5 or pure vector alone (verified with test fixtures)
- [ ] Context injection respects `MEMORY_CONTEXT_TOKEN_LIMIT` with ≤ 5% overage tolerance

### Binary & Runtime

- [ ] `cargo build --release` succeeds with `hydragent-embed` linked
- [ ] RAM usage with model loaded < 200 MB (measured on developer machine)
- [ ] All Phase 1 binary targets (`x86_64-linux`, `aarch64-linux`) still compile

### Documentation

- [ ] `README.md` updated with Phase 2 setup instructions (embedding model download)
- [ ] `ARCHITECTURE.md` updated with the 3-tier memory diagram
- [ ] `PHASE_2.md` (this file) reviewed and reflects actual implementation
- [ ] `config/standing_orders.md.example` committed with template content

### Release

- [ ] `v0.2.0` git tag created
- [ ] `CHANGELOG.md` entry for v0.2.0 written
- [ ] GitHub Release created with updated pre-built binaries

---

*Previous phase: [PHASE_1.md](PHASE_1.md) — Core Runtime Bootstrap (Weeks 1–6)*
*Next phase: [PHASE_3.md](PHASE_3.md) — WASM Sandbox, Encrypted Vault & 3-Tier Permission Matrix (Weeks 11–14)*
