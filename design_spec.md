# Hydragent Library & Graphify Design Spec (Core)

This specification details the core principles, data architecture, and cost-effective ingestion pipeline for Hydragent, based on the **Library Analogy** and the **Hermes Self-Improving Agent model**.

---

## 🏛️ 1. Conceptual Mapping (Library Analogy)

| Library Concept | Self-Improving Representation | Implementation Layer |
|---|---|---|
| **The Desk** | Active execution workspace (commands, skills, search, tool calls) | `react_loop.rs` |
| **Draft Paper** | Ephemeral context of the ongoing conversation | In-memory message list (not written to persistent SQLite tables until session ends) |
| **Page** | Condensed knowledgeable insights + User's personality/habit traits extracted from the session | `nodes` table (type = `"page"`) + `USER.md`/`SOUL.md` |
| **Book** | Topic clusters that compile related pages (e.g. "Aerospace", "AI", "Rust") | `nodes` table (type = `"book"`) |
| **Shelf** | Domain categorization clusters (e.g. "User's Area of Interest", "Way of Thinking") | `nodes` table (type = `"shelf"`) |
| **Web Connections** | Dynamic relationships mapping books to shelves, pages to books, and cross-references | `edges` table (generated/updated via `graphifyy`) |
| **Librarian** | The Hydragent core, performing actions, dreaming, and managing the library | `dream.rs` / `main.rs` |

---

## 🔌 2. Cost-Effective Ingestion Loop: 75% Graphify + 25% LLM

To protect the user's API budgets, we avoid relying entirely on LLMs to build and cluster the knowledge graph. We divide the labor to minimize costs:

```
[Draft Paper] ──► [Librarian (LLM - 25% Cost)] ──► Extracts Summary & Personality
                                                        │
                                            (Passes details to Graphify)
                                                        ▼
[Customized Graphify (Local - 75% Weight)] ◄────────────┘
        │
        ├─► Local AST parsing (finds code dependencies, file nodes)
        ├─► Graphify Clustering (computes Louvain communities for Books & Shelves)
        └─► Writes Page/Book/Shelf nodes and Edges directly to SQLite
```

### 1. LLM Role (25% Weight):
- **Summarization**: Compresses the ephemeral **Draft Paper** into a **Page** (insights summary).
- **Personality/Habits Extraction**: Extracts user personality markers, style habits, and behavior rules (to update `USER.md` and `SOUL.md` under strict character budget caps).

### 2. Customizing Graphify for Hydragent (75% Weight - Local & Code-First):
- **Document-Free Mode by Default**: We configure Graphify's file detector (`collect_files` in `detect.py`) to bypass raw documents and markdown files to completely eliminate redundant LLM extraction costs.
- **Dynamic Node Ingestion API**: We extend Graphify's `build.py` module to accept our live memory nodes (Pages, Books, Shelves) and user personality records rather than relying purely on filesystem files.
- **Community Clustering Overrides**: Graphify uses the Louvain community-detection algorithm. We customize this clustering step to automatically organize our generated **Page** nodes into **Books** (topics) and map those Books onto **Shelves** (domains) depending on shared tags and cross-references.

---

## 🗃️ 3. Relational vs Graph Storage: The Hybrid Query Bridge

To prevent the LLM from making duplicate queries, and to minimize execution time, we construct a **Unified Hybrid Query Bridge** in `crates/hydragent-memory/src/retrieval.rs`.

```
                  ┌──────────────────────────────┐
                  │      User Prompt / Query     │
                  └──────────────┬───────────────┘
                                 │
                                 ▼
               [Unified Memory Bridge (Local Retrieval)]
                                 │
         ┌───────────────────────┴───────────────────────┐
         ▼ (Step 1: Local SQLite FTS5)                   ▼ (Step 2: Graph Expansion)
  Finds matching Page Nodes                     Traverses neighbors of matched Pages
  using fast keyword index                      (Books & Shelves) for context
         │                                               │
         └───────────────────────┬───────────────────────┘
                                 │
                                 ▼
                     [Ranked Context Bubble]
                                 │ (Single Injection)
                                 ▼
                     [System Prompt Context]
```

### Performance & Token Optimizations:
1. **Parallel Local Search**: The SQLite keyword index match (FTS5) and the local Graphify AST/network traversal run in parallel using async tokio joins, completing in `< 10ms`.
2. **No-LLM Retrieval**: The bridge works entirely without LLM search steps.
3. **Single Injection**: By compiling Books, Shelves, and Pages into one ordered string, we prevent redundant context bloat, keeping prompt tokens small and fast to process.
