# Hydragent Memory System

Hydragent's memory is a library, but it is not a metaphor-only library. The implementation already has a real SQLite-backed session store, a typed graph library, vector search, FTS5 search, bounded personality files, and a dreaming pass that consolidates work into durable memory.

This design borrows the useful parts of OpenClaw, Hermes, and OpenCode:

- OpenClaw: keep the runtime lean and keep special capabilities in their own slots instead of making memory a catch-all.
- Hermes: keep persistent user/profile state, bounded skill and personality files, and a cleanup loop that runs automatically.
- OpenCode: keep admitted state separate from projected context, and make durable updates happen at explicit boundaries.

## 1. The Real Memory Layers

Hydragent memory is split into layers that already exist in the codebase.

| Hydragent Concept | What It Does | Implementation Layer |
|---|---|---|
| **The Desk** | Active execution and live tool use | `crates/hydragent-core/src/react_loop.rs` |
| **Draft Paper** | Uncommitted conversation in the current session | in-memory message flow before consolidation |
| **Page** | One condensed unit of durable knowledge | `messages`, `semantic_memories`, and `nodes(type='page')` |
| **Book** | Topic cluster made from related pages | `nodes(type='book')` |
| **Shelf** | Higher-level grouping of books | `nodes(type='shelf')` |
| **Web Connections** | Typed links between pages, books, and shelves | `edges` with `belongs_to`, `sits_on`, `cross_ref`, and `tag` relations |
| **Librarian** | The ingestion and consolidation worker | `crates/hydragent-core/src/dream.rs` and `crates/hydragent-memory/src/library.rs` |

The important rule is that memory is not one blob. Hydragent keeps a few different memory surfaces, each with a different purpose.

## 2. What Actually Gets Stored

Hydragent currently stores memory in three main forms:

- Session transcripts and consolidated messages in SQLite
- Semantic memories with embeddings and FTS5 support
- Library graph nodes and edges for Page / Book / Shelf structure

The `SessionStore` owns the durable SQLite schema. It stores:

- `messages` for session content and consolidation state
- `semantic_memories` for durable fact memory
- `semantic_memories_fts` for keyword retrieval
- `memory_tags` for fact tags
- `nodes` and `edges` for the typed library graph
- `page_meta` for page-level summary metadata
- bounded control tables such as `memory_consolidation_jobs`

That means Hydragent does not rely on a single giant memory table. It keeps the raw transcript, the semantic facts, and the graph structure separate so each layer can do one job well.

## 3. Retrieval Is Hybrid

Hydragent does not choose between keyword search and vector search. It combines them.

The `memory_search` tool calls `hydragent_memory::hybrid_search`, which does three local retrieval passes:

1. SQLite FTS5 keyword search over semantic memories
2. Graph expansion through the typed Library API
3. Vector similarity search through the in-memory HNSW index

The results are merged with reciprocal rank fusion and then decayed by age so newer, more relevant memories can outrank stale ones.

That is the real memory policy:

- FTS5 finds exact or near-exact text matches
- vector search finds semantically similar memories
- Graphify-style expansion pulls in related pages, books, and shelves

The result is compact, local, and cheap.

## 4. The Library Graph

Hydragent's typed graph is the core structure behind the library metaphor.

The `Library` type gives Hydragent three strict node kinds:

- `Page`
- `Book`
- `Shelf`

It also uses typed edge relations:

- `belongs_to` for page to book
- `sits_on` for book to shelf
- `cross_ref` for direct page links
- `tag` for deterministic tag markers used during clustering

This matters because the graph is not decorative. The graph is how Hydragent groups repeated experience into meaningful clusters and how it walks from a page to related context.

## 5. The Dreaming Cycle

Dreaming is Hydragent's consolidation pass.

It does three jobs:

1. Compress unconsolidated messages into durable facts and summaries
2. Keep `USER.md` and `SOUL.md` inside their character limits
3. Run the local graph clustering pass that turns pages into books and books into shelves

The actual implementation already separates the LLM and local work:

- the LLM extracts facts, style habits, and behavior rules
- the local graph layer performs clustering and edge creation
- a curator pass can run underneath dreaming when enabled

This is the Hydragent equivalent of a nightly clean-up worker. It should be thought of as compaction, not as a second chat loop.

## 6. Bounded Personality Memory

Hydragent keeps two special markdown files as durable personality memory:

- `USER.md` for user habits and communication preferences
- `SOUL.md` for agent behavior rules and tone

Both files are bounded by the implementation, not by wishful thinking.

- `USER.md` has a hard character limit
- `SOUL.md` has a hard character limit
- when a file exceeds its budget, the dreaming pass compacts it instead of appending forever

That gives Hydragent a stable personality layer without letting the files grow without limit.

## 7. Semantic Memory Rules

The semantic memory layer is a separate store for durable facts.

The code already enforces some useful rules:

- importance is clamped into a valid range before insert
- new facts are embedding-indexed and stored in SQLite
- deletions sweep the table, the FTS index, the tag table, and the vector store together
- low-importance or near-duplicate facts are skipped during consolidation

This is the right behavior for a memory system that wants to stay useful over time:

- keep important facts
- evict low-value facts first
- avoid duplicate paraphrases
- keep the FTS and vector views in sync with the source rows

## 8. Graphify in Practice

Graphify is not just a word in the docs. In Hydragent, it is the local graph layer that creates and traverses the Page / Book / Shelf structure.

The graph layer should be responsible for:

- upserting typed nodes
- linking pages to books and books to shelves
- connecting cross-references
- clustering pages by shared tags
- exposing a traversal layer for retrieval and context expansion

This is how Hydragent keeps memory structured instead of reducing everything to plain text search.

## 9. Identity Rules

The memory system must still feel like Hydragent.

- use desk, draft paper, pages, books, shelves, and Graphify
- keep the core runtime and the librarian separate in language and behavior
- keep raw transcripts, semantic facts, and graph structure distinct
- keep dreaming as compaction and curation, not as a generic batch job
- keep the memory layer local-first and SQLite-backed

The system can borrow strong ideas from the reference projects, but the user should still see Hydragent's own library-shaped memory vocabulary.

## 10. Suggested Memory State Machine

```text
live conversation
	-> draft paper
	-> consolidate messages
	-> extract facts and habits
	-> write semantic memories
	-> update USER.md / SOUL.md if needed
	-> cluster pages into books
	-> organize books onto shelves
	-> expose via hybrid search and graph expansion
```

## 11. Practical Defaults

- Keep the desk and draft paper ephemeral.
- Keep pages, books, and shelves durable.
- Use hybrid retrieval by default.
- Prefer bounded markdown files for personality memory.
- Prefer local graph expansion before asking the model for more context.
- Compact first, then grow the library.
