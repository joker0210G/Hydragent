# Phase 7: Hermes-Style Self-Improving Skill Engine, 7-Day Curator & LoRA Fine-Tuning Pipeline (Weeks 27–30)

> **Timeline**: Weeks 27–30
> **Theme**: Hydragent becomes **self-improving**. A Hermes-inspired skill engine learns reusable, composable programs from agent interaction logs. A **7-Day Curator** autonomously reviews, grades, and promotes or demotes skills on a weekly cycle. A **LoRA fine-tuning pipeline** lets the agent adapt a local language model to its own interaction patterns — making it measurably smarter at its specific tasks over time. A **benchmarking harness** tracks capability regression and improvement across every deployment cycle, with automated SKILL-BENCH scoring and golden-set evaluation.

> ## ❌ Implementation Status — Not Yet Implemented (Weeks 27–30, scheduled after Phase 6)
> 
> Cross-checked against [`doc/STATE.md`](../STATE.md) at `git rev 3d99366` (June 2026).
> 
> - **No `hydragent-skills` crate** and **no `hydragent-bench` crate** exist in the workspace.
> - `SkillSpec` YAML format, `SkillExtractor` (Hermes-style induction), `SkillExecutor` (ReAct subroutine replay), `SevenDayCurator`, `tools/finetune/` LoRA trainer, `SKILL-BENCH` task suite, and the **golden-set evaluator** are all described here but not built.
> - Skill composition / chaining (§5.9) and the observability dashboard (§5.10) are similarly unimplemented.
> - Files referenced by this doc that are not in the tree: `crates/hydragent-skills/`, `crates/hydragent-bench/`, `tools/finetune/`, `bench/`, `skills/`, `migrations/005_*.sql`.

---

## 📋 Table of Contents

1. [Phase Goals & Exit Criteria](#1-phase-goals--exit-criteria)
2. [Directory & Workspace Layout Changes](#2-directory--workspace-layout-changes)
3. [Technology Decisions](#3-technology-decisions)
4. [Week-by-Week Breakdown](#4-week-by-week-breakdown)
5. [Component Specifications](#5-component-specifications)
   - 5.1 [Skill Library Architecture & Schema](#51-skill-library-architecture--schema)
   - 5.2 [Skill Extractor (Hermes-Style Induction)](#52-skill-extractor-hermes-style-induction)
   - 5.3 [Skill Executor (ReAct Subroutine Runner)](#53-skill-executor-react-subroutine-runner)
   - 5.4 [7-Day Skill Curator](#54-7-day-skill-curator)
   - 5.5 [Synthetic Dataset Generator](#55-synthetic-dataset-generator)
   - 5.6 [LoRA Fine-Tuning Pipeline](#56-lora-fine-tuning-pipeline)
   - 5.7 [Benchmarking Harness & SKILL-BENCH](#57-benchmarking-harness--skill-bench)
   - 5.8 [Golden-Set Evaluator](#58-golden-set-evaluator)
   - 5.9 [Skill Composition & Chaining](#59-skill-composition--chaining)
   - 5.10 [Skill Observability Dashboard](#510-skill-observability-dashboard)
6. [Built-in Tools (Phase 7 Additions)](#6-built-in-tools-phase-7-additions)
7. [Configuration & Environment](#7-configuration--environment)
8. [Testing Strategy](#8-testing-strategy)
9. [Performance Targets](#9-performance-targets)
10. [Risks & Mitigations](#10-risks--mitigations)
11. [Definition of Done](#11-definition-of-done)

---

## 1. Phase Goals & Exit Criteria

Phase 7 is the **intelligence compounding phase** — where every interaction makes Hydragent incrementally better at its core tasks. Inspired by NovaSky-Berkeley's Hermes skill induction system (MIT license), OpenHands' CodeAct trajectory replay, and the LoRA fine-tuning approach demonstrated by Gemma-2-2B specialists.

### Hard Goals (must achieve before Phase 8)

| # | Goal | Validation |
|---|---|---|
| G1 | Skill extractor induces at least 1 reusable skill per 20 agent turns from interaction logs | Integration test: feed 100 synthetic turns → skill library grows by ≥ 5 new skills |
| G2 | Induced skills are stored in SQLite with versioning, capability tags, and success rate tracking | Unit test: insert skill v1 → update to v2 → query returns v2 with version history intact |
| G3 | Skill executor can replay an induced skill (ReAct subroutine) against a new input without invoking full orchestrator | Integration test: skill "convert-csv-to-json" replays against a fresh CSV → produces correct JSON |
| G4 | 7-Day Curator runs on schedule and promotes/demotes skills based on success rate | Integration test: seed skill with 3/10 success rate → curator runs → skill demoted to `inactive` |
| G5 | Synthetic dataset generator produces ≥ 100 fine-tuning examples from 1,000 interaction turns | Integration test: run `generate_dataset(session_ids)` → returns 100+ JSON examples in JSONL format |
| G6 | LoRA fine-tuning pipeline trains a local model for ≥ 1 epoch without OOM error | Integration test with mock `transformers` trainer; actual training verified on dev machine |
| G7 | Benchmarking harness reports SKILL-BENCH score for each model and stores history in SQLite | `cargo run --bin benchmark` produces JSON report; score history queryable via CLI |
| G8 | Golden-set evaluator: predefined Q&A pairs score ≥ 80% before `v0.7.0` tag | Run `./hydragent bench golden-set` → 80%+ pass rate verified |
| G9 | All Phase 1–6 tests remain green | `cargo test --workspace` and `pytest adapters/` both exit 0 |

### Soft Goals (target but not blocking)

- Skill marketplace stub: `skills/community/` directory for importing third-party skills
- A/B testing harness: route 10% of sessions to fine-tuned model; compare SKILL-BENCH score delta
- Skill dependency graph: skill A can call skill B as a subroutine (nested skill composition)
- `./hydragent skills list` CLI: shows all skills with success rate, usage count, and tier
- Automatic skill deduplication: if two skills have ≥ 90% semantic similarity, the lower-rated one is archived
- `./hydragent bench compare v0.6.0 v0.7.0` — compare benchmark scores across two release versions

---

## 2. Directory & Workspace Layout Changes

Phase 7 introduces `crates/hydragent-skills` (the skill engine) and `tools/finetune/` (the Python LoRA pipeline), plus a `bench/` workspace for the benchmarking harness.

```
hydragent/
│
├── crates/
│   │
│   ├── hydragent-core/                        # UPDATED
│   │   └── src/
│   │       ├── main.rs                        # UPDATED: skill engine init, curator spawn
│   │       └── orchestrator.rs               # UPDATED: skill lookup before ReAct fallback
│   │
│   ├── hydragent-skills/                     # NEW CRATE: skill engine
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── skill.rs                      # Skill struct, SkillVersion, SkillTier
│   │       ├── library.rs                    # SkillLibrary: SQLite CRUD for skills
│   │       ├── extractor.rs                  # SkillExtractor: Hermes induction from logs
│   │       ├── executor.rs                   # SkillExecutor: subroutine replay engine
│   │       ├── curator.rs                    # SevenDayCurator: weekly promotion/demotion
│   │       ├── composer.rs                   # SkillComposer: chains multiple skills
│   │       └── similarity.rs                # Semantic dedup via cosine similarity
│   │
│   ├── hydragent-bench/                      # NEW CRATE: benchmarking harness
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── harness.rs                    # BenchmarkHarness: SKILL-BENCH runner
│   │       ├── golden_set.rs                # GoldenSetEvaluator: Q&A answer grader
│   │       ├── metrics.rs                   # BenchmarkMetrics: scores, latency, cost
│   │       ├── history.rs                   # BenchmarkHistory: SQLite score timeline
│   │       └── reporter.rs                  # BenchmarkReporter: JSON/Markdown output
│   │
│   └── hydragent-types/                      # UPDATED
│       └── src/
│           └── lib.rs                        # UPDATED: Skill, SkillVersion, BenchmarkReport
│
├── tools/
│   └── finetune/                            # NEW: Python LoRA fine-tuning pipeline
│       ├── requirements.txt                 # transformers, peft, datasets, bitsandbytes, torch
│       ├── dataset_generator.py            # Converts interaction logs → JSONL training data
│       ├── lora_trainer.py                 # PEFT LoRA training script (configurable)
│       ├── model_evaluator.py              # Post-training evaluation vs baseline
│       └── config/
│           ├── lora_config.yaml            # LoRA hyperparameters (r, alpha, dropout, etc.)
│           └── training_config.yaml        # Training args: epochs, batch_size, lr, etc.)
│
├── bench/
│   ├── golden_set.jsonl                    # NEW: Curated Q&A evaluation set
│   ├── skill_bench_tasks.jsonl             # NEW: SKILL-BENCH task definitions
│   └── results/
│       └── {version}/
│           ├── report.json                 # Benchmark results per run
│           └── report.md                  # Human-readable benchmark report
│
├── skills/                                 # NEW: Skill storage directory
│   ├── builtin/                           # Shipped skills (maintained by team)
│   │   ├── convert_csv_to_json.yaml
│   │   ├── summarize_github_issue.yaml
│   │   └── debug_rust_error.yaml
│   ├── induced/                           # Auto-induced from interaction logs
│   │   └── {skill_id}.yaml               # Promoted skills with metadata
│   └── community/                         # Stub: third-party skills (Phase 8+)
│       └── .gitkeep
│
├── migrations/
│   ├── 001_initial.sql                    # Existing
│   ├── 002_hierarchical_memory.sql        # Phase 2
│   ├── 003_scheduler.sql                  # Phase 4
│   ├── 004_audit_chain.sql               # Phase 6
│   └── 005_skill_library.sql             # NEW Phase 7
│
└── tests/
    ├── unit/
    │   ├── skill_extractor_test.rs       # Hermes induction
    │   ├── skill_executor_test.rs        # Subroutine replay
    │   ├── curator_test.rs              # Promotion/demotion logic
    │   └── golden_set_test.rs           # Evaluator grading
    └── integration/
        ├── skill_induction_test.rs       # End-to-end: logs → skills
        ├── lora_dataset_test.py          # Dataset generator output
        └── benchmark_harness_test.rs     # SKILL-BENCH runner
```

---

## 3. Technology Decisions

---

### 3.1 Language Roles in Phase 7

| Component | Language | Rationale |
|---|---|---|
| Skill library CRUD, extractor, executor, curator | **Rust** | Performance-critical; called on every agent turn for skill lookup |
| Benchmarking harness, golden-set evaluator | **Rust** | Deterministic; outputs structured JSON; no ML needed |
| Dataset generator | **Python** | Rich text processing; `datasets` library for JSONL generation |
| LoRA fine-tuning | **Python** | `transformers` + `peft` + `bitsandbytes` are Python-only; no viable Rust alternative |
| Model evaluator (post-training) | **Python** | Wraps Rust benchmark harness via subprocess; parses JSON report |

---

### 3.2 Skill Representation: Why YAML + SQLite?

A skill must be both human-readable (for inspection and curation) and machine-queryable (for fast lookup). Two data stores serve different access patterns:

| Use Case | Format | Rationale |
|---|---|---|
| Human inspection of a skill | YAML file in `skills/` | Readable; can be version-controlled in git |
| Fast lookup by capability tags | SQLite with FTS5 | Sub-millisecond search across 1,000+ skills |
| Embedding-based similarity search | HNSW (Phase 2 VectorStore) | Find similar skills for deduplication and composition |
| Version history | SQLite `skill_versions` table | Full audit trail of every skill change |

**Decision**: SQLite as the source of truth; YAML files in `skills/` as human-readable exports. Skills are exported to YAML on promotion and imported from YAML for builtins.

---

### 3.3 Hermes-Style Skill Induction vs. Plain Prompt Template

| Approach | Hermes-Style Induction | Plain Prompt Template |
|---|---|---|
| **What it captures** | Full ReAct trajectory (Thought → Action → Observation loop) | Only input/output pairs |
| **Reusability** | Can replay with different inputs by substituting parameters | Input-specific; cannot generalize |
| **Composability** | Can chain skills: A's output → B's input | Static; no chaining |
| **Self-improvement** | Success rate tracked; skill promoted/demoted based on real performance | No feedback loop |
| **Representation** | Parameterized ReAct program with `{{var}}` placeholders | Fixed string template |

**Decision**: Hermes-style induction. The `SkillExtractor` identifies successful ReAct trajectories and parameterizes them into reusable programs.

---

### 3.4 LoRA vs. Full Fine-Tuning vs. RAG for Self-Improvement

| Approach | LoRA Fine-Tuning | Full Fine-Tuning | Retrieval-Augmented (Phase 2) |
|---|---|---|---|
| **Hardware requirement** | 8–24 GB VRAM (with 4-bit quantization) | 80–320 GB VRAM | CPU-only (already implemented) |
| **Catastrophic forgetting** | Low (LoRA adapters are additive) | High | None (retrieval is separate) |
| **Deployment artifact** | 1–50 MB adapter file | Multi-GB model file | No new artifact |
| **Personalization depth** | Deep (weight-level adaptation) | Deep | Shallow (surface retrieval) |
| **Training time (7B model)** | ~2h on A100 for 1 epoch | ~48h on 8×A100 | None |

**Decision**: LoRA for Phase 7. The pipeline is optional (not required for the agent to function) but provides measurable improvement on user-specific tasks. Users without GPUs can skip this step — RAG (Phase 2) provides sufficient personalization for most use cases.

---

### 3.5 LoRA Library: `peft` vs. `trl` vs. `llama-factory`

| Library | Stars | Pros | Cons | Choice |
|---|---|---|---|---|
| **`peft`** (HuggingFace) | 17k ⭐ | Official HuggingFace; LoRA, QLoRA; integrates with `transformers` | Less opinionated; requires more config | ✅ **Chosen** |
| **`trl`** (HuggingFace) | 10k ⭐ | Great for RLHF/DPO; `SFTTrainer` is convenient | More complex for simple SFT | Secondary (used for `SFTTrainer`) |
| **`llama-factory`** | 39k ⭐ | Easiest setup; Web UI | Opaque; hard to customize data pipeline | ❌ Too opinionated |

**Decision**: `peft` (for `LoraConfig`) + `trl` (for `SFTTrainer` for Supervised Fine-Tuning) + `bitsandbytes` (for 4-bit QLoRA).

---

### 3.6 SKILL-BENCH Design

SKILL-BENCH is our internal benchmark, inspired by HumanEval (code), GAIA (general agent tasks), and SWE-bench (software engineering). It tests Hydragent's core capabilities:

| Category | Number of Tasks | Scoring |
|---|---|---|
| Information retrieval (web search) | 20 | 0/1 (answer contains required fact) |
| Code generation (write a Rust fn) | 15 | 0/1 (code compiles + passes test case) |
| Fact recall from memory | 10 | 0/1 (recalls stored fact exactly) |
| Tool orchestration (multi-step) | 15 | Partial (0.5 for attempting correct tools, 1.0 for correct answer) |
| Reasoning (math, logic) | 10 | 0/1 (correct final answer) |
| Skill execution (replay a skill) | 10 | 0/1 (skill output matches golden answer ± 5% numeric tolerance) |
| **Total** | **80 tasks** | **80 points max (normalized to 0–100%)** |

---

## 4. Week-by-Week Breakdown

### Week 27 — Skill Library & Hermes Skill Induction

**Goal**: Successful agent trajectories are automatically converted into reusable, parameterized skills stored in SQLite.

| Day | Task |
|---|---|
| Mon | Write `migrations/005_skill_library.sql`. Tables: `skills`, `skill_versions`, `skill_tags`, `skill_executions`. Create `crates/hydragent-skills` crate. Define `Skill`, `SkillVersion`, `SkillTier` structs in `skill.rs`. |
| Tue | Implement `SkillLibrary` in `library.rs`: `insert_skill()`, `get_skill(id)`, `list_skills()`, `update_skill()`, `search_by_tag(tag)`, `search_by_embedding(vec, k)`. Wire FTS5 on `name || description` for keyword search. |
| Wed | Design the `SkillSpec` YAML format (Section 5.1). Implement YAML serialization/deserialization for `Skill`. Implement `SkillLibrary::export_to_yaml(skill_id, path)` and `import_from_yaml(path)`. Load `skills/builtin/*.yaml` at startup. |
| Thu | Implement `SkillExtractor` in `extractor.rs`. Craft the Hermes-style extraction LLM prompt (Section 5.2). `extract_from_trajectory(turns: &[Turn]) -> Option<SkillCandidate>`. Parse LLM response into `SkillCandidate` JSON. |
| Fri | Implement parameter detection in `SkillExtractor`: identify variable parts of the trajectory (e.g., file names, URLs, user queries) and replace them with `{{param_name}}` placeholders. Build `SkillSpec.params` list with type and description. |
| Sat | Wire skill extraction into the Dreaming pipeline (Phase 2): after consolidating messages, run `SkillExtractor` on each successful ReAct trajectory. Store `SkillTier::Candidate` in `skills`. |
| Sun | Unit tests: (1) 10-turn successful CSV conversion trajectory → extracted as "csv_to_json" skill with `{{input_csv}}` and `{{output_format}}` params; (2) failed trajectory → `None` (not extracted); (3) YAML round-trip preserves all skill fields. |

**Deliverable**: `cargo test -p hydragent-skills` green. Builtin skills loaded at startup. Dreaming integration induces candidate skills.

---

### Week 28 — Skill Executor & 7-Day Curator

**Goal**: Induced skills can be replayed. The curator promotes successful skills and retires failing ones.

| Day | Task |
|---|---|
| Mon | Implement `SkillExecutor` in `executor.rs`. Takes a `Skill` + `HashMap<String, String>` params + `input`. Substitutes `{{param}}` placeholders, renders the trajectory template, and runs it through a mini ReAct loop (`max_steps=5`). Returns `SkillExecutionResult`. |
| Tue | Implement skill parameter substitution: template engine supporting `{{param}}` — simple find-replace. Validate all required params are present before execution. Emit `SkillExecutionError::MissingParam(name)` if any missing. |
| Wed | Wire skill lookup into `orchestrator.rs`: before building a full ReAct loop, call `skill_library.find_matching_skill(user_message)` — uses FTS5 + embedding similarity. If a high-confidence match found (similarity > 0.85), run via `SkillExecutor` first. Log skill match in `tracing::info!`. |
| Thu | Implement `SevenDayCurator` in `curator.rs`. Scheduled via `CronScheduler` (Phase 4) with `"0 3 * * 0"` (3 AM Sunday). Loads all skills with `executions_last_7_days`. Computes `success_rate = successes / total`. |
| Fri | Implement curation decisions: `success_rate >= 0.8` → `SkillTier::Active`; `0.5–0.8` → `SkillTier::Candidate` (keep, needs more data); `< 0.5 AND total >= 5` → `SkillTier::Inactive`; `0 executions in 7 days` → `SkillTier::Archived`. |
| Sat | Implement curator notifications: after each weekly run, push a heartbeat (Phase 4) to the configured channel with a digest: "This week: 3 skills promoted, 1 demoted, 2 archived". |
| Sun | Integration test: seed 10 candidate skills with synthetic execution records. Run curator manually. Assert: high-success skills are `Active`, low-success are `Inactive`, zero-use are `Archived`. |

**Deliverable**: Skills are automatically promoted/demoted weekly. `SkillExecutor` replays skills correctly. Orchestrator prefers skills over full ReAct for matching queries.

---

### Week 29 — Synthetic Dataset Generation & LoRA Fine-Tuning

**Goal**: The agent generates its own fine-tuning training data and trains a LoRA adapter on a local model.

| Day | Task |
|---|---|
| Mon | Implement `tools/finetune/dataset_generator.py`: `generate_dataset(session_ids, output_path)`. Queries `messages` table for `requires_consolidation=0` turns. Filters for successful ReAct turns (tool_result status = success). Formats as `{"messages": [{"role": ..., "content": ...}]}` JSONL. |
| Tue | Implement quality filtering in `dataset_generator.py`: only include turns where the agent used ≥ 1 tool correctly AND the user did not follow up with a correction ("actually", "no, that's wrong", "try again"). Validate JSONL output via JSON schema. |
| Wed | Implement `tools/finetune/lora_trainer.py`: load a base model (default: `google/gemma-2-2b-it`) with 4-bit quantization via `bitsandbytes`. Apply `LoraConfig` from `config/lora_config.yaml`. Run `SFTTrainer` on the generated JSONL dataset. |
| Thu | Implement training checkpointing: save adapter every `save_steps` (default: 100). Support `--resume` from latest checkpoint. Export final adapter to `data/models/lora_adapters/{run_id}/`. |
| Fri | Implement `tools/finetune/model_evaluator.py`: after training, run the fine-tuned model against the golden set and SKILL-BENCH. Compare scores vs. baseline model. Output comparison report. |
| Sat | Implement `./hydragent finetune start` CLI subcommand: triggers `dataset_generator.py` + `lora_trainer.py` + `model_evaluator.py` as subprocess pipeline. Streams progress to terminal. |
| Sun | Integration test with mock trainer: `generate_dataset` on 200 synthetic turns → verify ≥ 20 JSONL examples output → verify schema valid → verify no Secret-tainted data included (taint check before export). |

**Deliverable**: `generate_dataset` produces valid JSONL. `lora_trainer.py` runs without error on mock dataset. CLI works end-to-end.

---

### Week 30 — Benchmarking Harness, Golden Set & Phase 7 Release

**Goal**: SKILL-BENCH score measured. Golden-set passes 80%. Phase 7 tagged.

| Day | Task |
|---|---|
| Mon | Implement `BenchmarkHarness` in `harness.rs`. Load `bench/skill_bench_tasks.jsonl`. For each task: submit to orchestrator via internal API; compare response to expected answer; score. Collect `BenchmarkMetrics`. |
| Tue | Implement `GoldenSetEvaluator` in `golden_set.rs`. Load `bench/golden_set.jsonl`. For each Q&A pair: call `orchestrator.process(question)` → compare to `expected_answer`. Scoring: exact match (1.0), partial match (0.5 via `jaro_winkler` similarity), mismatch (0.0). |
| Wed | Implement `BenchmarkHistory` in `history.rs`: SQLite table `benchmark_runs`. Stores: `run_id`, `version`, `score`, `timestamp`, `metrics_json`. `./hydragent bench history` CLI shows all past runs with trend arrow (↑/↓/=). |
| Thu | Write 80 SKILL-BENCH task definitions in `bench/skill_bench_tasks.jsonl` covering all 6 categories (Section 3.6). Write 30 golden-set Q&A pairs in `bench/golden_set.jsonl`. Ensure golden set covers: facts stored in memory, web search facts, reasoning tasks. |
| Fri | Implement `BenchmarkReporter`: `generate_json_report(metrics)` outputs to `bench/results/{version}/report.json`; `generate_markdown_report(metrics)` outputs `report.md` with tables and trend charts (ASCII). |
| Sat | Phase 7 full regression: `cargo test --workspace` + `pytest adapters/ -v`. Run `./hydragent bench golden-set` → verify ≥ 80% pass rate. Fix any failures. |
| Sun | Tag `v0.7.0`. Write CHANGELOG. Update `ARCHITECTURE.md` with skill engine layer. Publish benchmark results in GitHub Release. |

**Deliverable**: `v0.7.0` tag. SKILL-BENCH baseline score recorded. Golden-set ≥ 80%.

---

## 5. Component Specifications

### 5.1 Skill Library Architecture & Schema

#### 5.1.1 SkillSpec YAML Format

Every skill, whether builtin or induced, is represented as a YAML file:

```yaml
# skills/builtin/debug_rust_error.yaml

id: "skill-builtin-debug-rust-error"
name: "Debug Rust Compiler Error"
version: 1
description: "Analyzes a Rust compiler error and suggests a fix."
tier: "active"
capability_tags:
  - "rust"
  - "debugging"
  - "compiler"
author: "builtin"
created_at: "2026-01-01T00:00:00Z"
last_updated: "2026-01-01T00:00:00Z"
success_rate: 0.91
execution_count: 127

params:
  - name: "error_message"
    type: "string"
    description: "The full Rust compiler error output (rustc stderr)"
    required: true
  - name: "source_file"
    type: "string"
    description: "The Rust source file content (optional, improves accuracy)"
    required: false

trajectory:
  - role: "thought"
    content: "I need to analyze this Rust compiler error: {{error_message}}"
  - role: "action"
    tool: "memory_search"
    params:
      query: "Rust compiler error {{error_message | first_line}}"
  - role: "thought"
    content: "Based on the error pattern, I can identify the root cause."
  - role: "response"
    template: |
      **Rust Compiler Error Analysis**

      **Error**: `{{error_message | first_line}}`

      **Root Cause**: [Inferred from the error type]

      **Fix**:
      ```rust
      [Suggested code fix]
      ```

test_cases:
  - input: "error[E0382]: borrow of moved value: `x`"
    expected_contains: ["borrow", "ownership", "clone"]
```

#### 5.1.2 Database Schema

```sql
-- migrations/005_skill_library.sql

CREATE TABLE IF NOT EXISTS skills (
    id                TEXT    PRIMARY KEY,          -- UUID or "skill-builtin-{name}"
    name              TEXT    NOT NULL UNIQUE,      -- Human-readable name (unique across library)
    description       TEXT    NOT NULL,
    tier              TEXT    NOT NULL
        CHECK (tier IN ('candidate', 'active', 'inactive', 'archived')),
    author            TEXT    NOT NULL DEFAULT 'induced',
    created_at        INTEGER NOT NULL,
    last_updated      INTEGER NOT NULL,
    execution_count   INTEGER NOT NULL DEFAULT 0,
    success_count     INTEGER NOT NULL DEFAULT 0,
    failure_count     INTEGER NOT NULL DEFAULT 0,
    success_rate      REAL    NOT NULL DEFAULT 0.0,  -- Recomputed after each execution
    source_session_id TEXT,                          -- Session from which this skill was induced
    embedding_id      TEXT                           -- Reference to HNSW vector for similarity search
);

CREATE INDEX IF NOT EXISTS idx_skills_tier        ON skills(tier);
CREATE INDEX IF NOT EXISTS idx_skills_success_rate ON skills(success_rate DESC);
CREATE INDEX IF NOT EXISTS idx_skills_updated     ON skills(last_updated DESC);

-- FTS5 for keyword-based skill lookup
CREATE VIRTUAL TABLE IF NOT EXISTS skills_fts
USING fts5(
    name,
    description,
    content='skills',
    content_rowid='id',
    tokenize='trigram'
);

-- Auto-sync triggers
CREATE TRIGGER skills_ai AFTER INSERT ON skills BEGIN
    INSERT INTO skills_fts(rowid, name, description) VALUES (new.id, new.name, new.description);
END;
CREATE TRIGGER skills_ad AFTER DELETE ON skills BEGIN
    INSERT INTO skills_fts(skills_fts, rowid, name, description)
        VALUES ('delete', old.id, old.name, old.description);
END;
CREATE TRIGGER skills_au AFTER UPDATE ON skills BEGIN
    INSERT INTO skills_fts(skills_fts, rowid, name, description)
        VALUES ('delete', old.id, old.name, old.description);
    INSERT INTO skills_fts(rowid, name, description) VALUES (new.id, new.name, new.description);
END;

-- Version history
CREATE TABLE IF NOT EXISTS skill_versions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    skill_id     TEXT    NOT NULL,
    version      INTEGER NOT NULL,
    spec_yaml    TEXT    NOT NULL,  -- Full YAML at this version
    changed_by   TEXT    NOT NULL,  -- "curator", "user", "extractor"
    change_note  TEXT,
    created_at   INTEGER NOT NULL,
    FOREIGN KEY(skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_skill_versions_unique ON skill_versions(skill_id, version);

-- Per-execution records for curator analysis
CREATE TABLE IF NOT EXISTS skill_executions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    skill_id        TEXT    NOT NULL,
    session_id      TEXT,
    executed_at     INTEGER NOT NULL,
    success         BOOLEAN NOT NULL,
    execution_ms    INTEGER,
    error_message   TEXT,
    input_hash      TEXT,   -- SHA-256 of input params (for dedup without storing PII)
    FOREIGN KEY(skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_skill_executions_skill ON skill_executions(skill_id, executed_at DESC);
CREATE INDEX IF NOT EXISTS idx_skill_executions_recent ON skill_executions(executed_at DESC);

-- Capability tags
CREATE TABLE IF NOT EXISTS skill_tags (
    skill_id TEXT NOT NULL,
    tag      TEXT NOT NULL,
    PRIMARY KEY(skill_id, tag),
    FOREIGN KEY(skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_skill_tags_tag ON skill_tags(tag);
```

---

### 5.2 Skill Extractor (Hermes-Style Induction)

```rust
// crates/hydragent-skills/src/extractor.rs

use hydragent_model::ModelProvider;
use crate::skill::{Skill, SkillParam, SkillStep, SkillTier};

/// Hermes-style skill induction from successful ReAct trajectories.
///
/// Inspiration: NovaSky-Berkeley's Hermes framework.
/// A trajectory is "successful" if:
/// 1. All tool calls returned ToolStatus::Success
/// 2. The user did not send a correction message after the agent's response
/// 3. The trajectory involved at least 2 distinct tool calls (non-trivial)
pub struct SkillExtractor {
    llm: Arc<dyn ModelProvider>,
}

const EXTRACTION_SYSTEM_PROMPT: &str = r#"
You are a skill extraction expert for an AI agent system. Your job is to analyze a successful
agent trajectory and extract a reusable, parameterized skill program.

A SKILL is a trajectory template where variable parts are replaced with {{parameter_name}} placeholders.

EXTRACTION RULES:
1. Identify the GOAL of the trajectory in one sentence (becomes the skill name).
2. Identify VARIABLE parts — things that would change if the skill were applied to a similar task:
   - File names, URLs, usernames → {{file_path}}, {{url}}, {{username}}
   - Query strings, search terms → {{query}}, {{search_term}}
   - Domain-specific content → {{input_data}}, {{target_value}}
3. Extract the trajectory as a sequence of steps, with {{param}} substitutions applied.
4. For each parameter, provide:
   - name: snake_case identifier
   - type: string | integer | boolean | list
   - description: what this parameter represents
   - required: true/false
5. Skills MUST be general enough to apply to at least 3 different similar tasks.
6. If the trajectory is too specific to generalize, output: {"extractable": false}

OUTPUT: Valid JSON only. No markdown.

SCHEMA:
{
  "extractable": true,
  "name": "short-kebab-case-name",
  "description": "One sentence describing what this skill does",
  "capability_tags": ["tag1", "tag2"],
  "params": [
    {"name": "param_name", "type": "string", "description": "what it is", "required": true}
  ],
  "steps": [
    {"role": "thought", "content": "Thought with {{param}} substituted"},
    {"role": "action", "tool": "tool_name", "params_template": {"key": "{{param}}"}},
    {"role": "response", "template": "Response with {{param}}"}
  ]
}
"#;

#[derive(Debug, serde::Deserialize)]
pub struct SkillCandidate {
    pub extractable: bool,
    pub name: Option<String>,
    pub description: Option<String>,
    pub capability_tags: Option<Vec<String>>,
    pub params: Option<Vec<SkillParam>>,
    pub steps: Option<Vec<SkillStep>>,
}

impl SkillExtractor {
    pub fn new(llm: Arc<dyn ModelProvider>) -> Self { Self { llm } }

    /// Attempt to extract a skill from a sequence of conversation turns.
    /// Returns `None` if the trajectory is not suitable for extraction.
    pub async fn extract_from_trajectory(
        &self,
        turns: &[ConversationTurn],
        session_id: &str,
    ) -> anyhow::Result<Option<SkillCandidate>> {
        // Qualify the trajectory before calling LLM (save cost)
        if !self.is_extractable(turns) {
            tracing::debug!(session_id, "Trajectory not eligible for skill extraction");
            return Ok(None);
        }

        let trajectory_text = self.format_trajectory(turns);

        let prompt = format!(
            "{}\n\nTRAJECTORY TO ANALYZE:\n{}\n\nOUTPUT JSON:",
            EXTRACTION_SYSTEM_PROMPT,
            trajectory_text
        );

        let raw_response = self.llm.generate_non_streaming(&prompt).await?;

        // Extract JSON from response
        let json_str = extract_json_from_response(&raw_response)
            .map_err(|e| anyhow::anyhow!("Skill extraction LLM returned non-JSON: {}", e))?;

        let candidate: SkillCandidate = serde_json::from_str(&json_str)
            .map_err(|e| anyhow::anyhow!("Failed to parse skill candidate JSON: {}", e))?;

        if !candidate.extractable {
            tracing::debug!(session_id, "LLM determined trajectory is not extractable");
            return Ok(None);
        }

        tracing::info!(
            session_id,
            skill_name = candidate.name.as_deref().unwrap_or("unnamed"),
            "Skill candidate extracted"
        );

        Ok(Some(candidate))
    }

    /// Check eligibility before paying for an LLM call.
    fn is_extractable(&self, turns: &[ConversationTurn]) -> bool {
        let tool_calls: Vec<_> = turns.iter()
            .filter(|t| t.role == "tool_use")
            .collect();

        let successful_tool_calls = tool_calls.iter()
            .filter(|t| t.tool_status.as_deref() == Some("success"))
            .count();

        let distinct_tools: std::collections::HashSet<_> = tool_calls.iter()
            .filter_map(|t| t.tool_id.as_ref())
            .collect();

        let user_correction = turns.iter()
            .filter(|t| t.role == "user")
            .skip(1) // Skip the first user message (the original request)
            .any(|t| {
                let lc = t.content.to_lowercase();
                ["actually", "no, that", "wrong", "try again", "incorrect"]
                    .iter()
                    .any(|kw| lc.contains(kw))
            });

        // Must have: ≥ 2 successful tool calls, ≥ 2 distinct tools, no corrections
        successful_tool_calls >= 2 && distinct_tools.len() >= 2 && !user_correction
    }

    fn format_trajectory(&self, turns: &[ConversationTurn]) -> String {
        turns.iter()
            .map(|t| format!("[{}] {}", t.role.to_uppercase(), t.content))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}
```

---

### 5.3 Skill Executor (ReAct Subroutine Runner)

```rust
// crates/hydragent-skills/src/executor.rs

use std::collections::HashMap;
use hydragent_types::{ToolCall, ToolResult};
use crate::skill::{Skill, SkillStep};

#[derive(Debug)]
pub struct SkillExecutionResult {
    pub success: bool,
    pub output: String,
    pub steps_taken: usize,
    pub execution_ms: u64,
    pub error: Option<String>,
}

pub struct SkillExecutor {
    tool_registry: Arc<ToolRegistry>,
    max_steps: usize,
}

impl SkillExecutor {
    pub fn new(tool_registry: Arc<ToolRegistry>) -> Self {
        Self { tool_registry, max_steps: 5 }
    }

    /// Execute a skill with the given parameter bindings.
    ///
    /// This is a "mini ReAct loop" — it replays the skill's trajectory template
    /// with `{{param}}` substituted, but allows the agent to deviate at Observation steps.
    pub async fn execute(
        &self,
        skill: &Skill,
        params: HashMap<String, String>,
        input: &str,
    ) -> anyhow::Result<SkillExecutionResult> {
        let start = std::time::Instant::now();

        // Validate required params
        for param_spec in &skill.params {
            if param_spec.required && !params.contains_key(&param_spec.name) {
                return Ok(SkillExecutionResult {
                    success: false,
                    output: String::new(),
                    steps_taken: 0,
                    execution_ms: 0,
                    error: Some(format!("Missing required parameter: '{}'", param_spec.name)),
                });
            }
        }

        let mut steps_taken = 0;
        let mut final_output = String::new();
        let mut context: HashMap<String, String> = params.clone();
        context.insert("input".to_string(), input.to_string());

        for step in &skill.trajectory {
            if steps_taken >= self.max_steps {
                break;
            }

            match step.role.as_str() {
                "thought" => {
                    let thought = self.substitute_params(&step.content, &context);
                    tracing::debug!(thought = %thought, "Skill thought step");
                    steps_taken += 1;
                }

                "action" => {
                    let tool_id = step.tool.as_deref().unwrap_or("echo");
                    let params_json = step.params_template
                        .as_ref()
                        .map(|tmpl| {
                            let rendered: serde_json::Value = match tmpl {
                                serde_json::Value::Object(map) => {
                                    let substituted: serde_json::Map<String, serde_json::Value> = map.iter()
                                        .map(|(k, v)| {
                                            let v_str = v.as_str().unwrap_or("");
                                            let v_rendered = self.substitute_params(v_str, &context);
                                            (k.clone(), serde_json::Value::String(v_rendered))
                                        })
                                        .collect();
                                    serde_json::Value::Object(substituted)
                                }
                                other => other.clone(),
                            };
                            rendered.to_string()
                        })
                        .unwrap_or_default();

                    let call = ToolCall {
                        call_id: uuid::Uuid::new_v4().to_string(),
                        tool_id: tool_id.to_string(),
                        params_json,
                    };

                    match self.tool_registry.invoke(&call).await {
                        result if result.status == ToolStatus::Success => {
                            // Inject tool result into context for subsequent steps
                            context.insert(
                                format!("{}_result", tool_id.replace('-', "_")),
                                result.output_json.clone(),
                            );
                            tracing::debug!(tool_id, "Skill action step succeeded");
                        }
                        result => {
                            let error_msg = result.error_message
                                .unwrap_or_else(|| "Tool call failed".to_string());
                            return Ok(SkillExecutionResult {
                                success: false,
                                output: String::new(),
                                steps_taken,
                                execution_ms: start.elapsed().as_millis() as u64,
                                error: Some(format!("Tool '{}' failed: {}", tool_id, error_msg)),
                            });
                        }
                    }

                    steps_taken += 1;
                }

                "response" => {
                    let template = step.template.as_deref().unwrap_or(&step.content);
                    final_output = self.substitute_params(template, &context);
                    steps_taken += 1;
                    break; // Response is always the last step
                }

                unknown => {
                    tracing::warn!(role = %unknown, "Unknown skill step role — skipping");
                }
            }
        }

        let execution_ms = start.elapsed().as_millis() as u64;
        let success = !final_output.is_empty();

        tracing::info!(
            skill_id = %skill.id,
            skill_name = %skill.name,
            success,
            steps_taken,
            execution_ms,
            "Skill execution complete"
        );

        Ok(SkillExecutionResult { success, output: final_output, steps_taken, execution_ms, error: None })
    }

    /// Substitute `{{param_name}}` placeholders in a template string.
    fn substitute_params(&self, template: &str, context: &HashMap<String, String>) -> String {
        let mut result = template.to_string();
        for (key, value) in context {
            result = result.replace(&format!("{{{{{}}}}}", key), value);
            // Support filter syntax: {{param | first_line}}
            let first_line_placeholder = format!("{{{{{}|first_line}}}}", key);
            let first_line_value = value.lines().next().unwrap_or(value);
            result = result.replace(&first_line_placeholder, first_line_value);
        }
        result
    }
}
```

---

### 5.4 7-Day Skill Curator

```rust
// crates/hydragent-skills/src/curator.rs

use crate::library::SkillLibrary;
use crate::skill::SkillTier;
use hydragent_scheduler::HeartbeatEngine;
use sqlx::SqlitePool;

pub struct SevenDayCurator {
    library: Arc<SkillLibrary>,
    heartbeat: Arc<HeartbeatEngine>,
    target_channel: String,  // Where to push curator digest
    config: CuratorConfig,
}

#[derive(Debug, Clone)]
pub struct CuratorConfig {
    /// Success rate above which a candidate is promoted to Active
    pub promote_threshold: f64,
    /// Success rate below which (with min executions) a skill is demoted to Inactive
    pub demote_threshold: f64,
    /// Minimum executions required before demotion is allowed
    pub min_executions_for_demotion: u32,
    /// Days of inactivity before a skill is archived
    pub archive_inactive_days: u32,
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            promote_threshold: 0.80,
            demote_threshold: 0.50,
            min_executions_for_demotion: 5,
            archive_inactive_days: 30,
        }
    }
}

#[derive(Debug, Default)]
pub struct CurationReport {
    pub promoted: Vec<String>,
    pub demoted: Vec<String>,
    pub archived: Vec<String>,
    pub no_change: Vec<String>,
    pub run_timestamp: i64,
}

impl SevenDayCurator {
    pub async fn run_weekly_curation(&self) -> anyhow::Result<CurationReport> {
        let now = chrono::Utc::now().timestamp_millis();
        let seven_days_ago = now - (7 * 24 * 60 * 60 * 1000);

        tracing::info!("7-Day Curator: starting weekly skill curation");

        // Load all non-archived skills with their recent execution stats
        let skills = self.library.list_skills_with_stats(seven_days_ago).await?;

        let mut report = CurationReport {
            run_timestamp: now,
            ..Default::default()
        };

        for skill_stats in skills {
            let new_tier = self.evaluate_skill(&skill_stats, now);

            if new_tier != skill_stats.tier {
                self.library.update_tier(&skill_stats.id, new_tier.clone()).await?;

                match new_tier {
                    SkillTier::Active => {
                        report.promoted.push(skill_stats.name.clone());
                        tracing::info!(
                            skill = %skill_stats.name,
                            success_rate = skill_stats.success_rate_7d,
                            "Skill PROMOTED to Active"
                        );
                    }
                    SkillTier::Inactive => {
                        report.demoted.push(skill_stats.name.clone());
                        tracing::info!(
                            skill = %skill_stats.name,
                            success_rate = skill_stats.success_rate_7d,
                            executions = skill_stats.executions_7d,
                            "Skill DEMOTED to Inactive"
                        );
                    }
                    SkillTier::Archived => {
                        report.archived.push(skill_stats.name.clone());
                        tracing::info!(skill = %skill_stats.name, "Skill ARCHIVED (unused)");
                    }
                    SkillTier::Candidate => {
                        report.no_change.push(skill_stats.name.clone());
                    }
                }
            } else {
                report.no_change.push(skill_stats.name.clone());
            }
        }

        // Push digest to configured channel
        self.push_curation_digest(&report).await?;

        tracing::info!(
            promoted = report.promoted.len(),
            demoted = report.demoted.len(),
            archived = report.archived.len(),
            "7-Day Curator: curation complete"
        );

        Ok(report)
    }

    fn evaluate_skill(&self, stats: &SkillWithStats, now_ms: i64) -> SkillTier {
        let days_since_last_exec = stats.last_executed_ms
            .map(|t| (now_ms - t) / (24 * 60 * 60 * 1000))
            .unwrap_or(i64::MAX);

        // Archive if not used in `archive_inactive_days` days
        if days_since_last_exec >= self.config.archive_inactive_days as i64 {
            return SkillTier::Archived;
        }

        // Not enough data for promotion/demotion decisions
        if stats.executions_7d < self.config.min_executions_for_demotion {
            return SkillTier::Candidate;
        }

        if stats.success_rate_7d >= self.config.promote_threshold {
            SkillTier::Active
        } else if stats.success_rate_7d < self.config.demote_threshold {
            SkillTier::Inactive
        } else {
            SkillTier::Candidate
        }
    }

    async fn push_curation_digest(&self, report: &CurationReport) -> anyhow::Result<()> {
        let mut lines = vec!["🧠 **Weekly Skill Curation Report**\n".to_string()];

        if !report.promoted.is_empty() {
            lines.push(format!("✅ **Promoted** ({}):", report.promoted.len()));
            for name in &report.promoted { lines.push(format!("  • {}", name)); }
        }
        if !report.demoted.is_empty() {
            lines.push(format!("\n⚠️ **Demoted** ({}):", report.demoted.len()));
            for name in &report.demoted { lines.push(format!("  • {}", name)); }
        }
        if !report.archived.is_empty() {
            lines.push(format!("\n📦 **Archived** ({}):", report.archived.len()));
            for name in &report.archived { lines.push(format!("  • {}", name)); }
        }
        if !report.no_change.is_empty() {
            lines.push(format!("\n— **Unchanged**: {} skills", report.no_change.len()));
        }

        let digest = lines.join("\n");
        self.heartbeat.push(&self.target_channel, "system:curator", &digest).await?;
        Ok(())
    }
}
```

---

### 5.5 Synthetic Dataset Generator

```python
# tools/finetune/dataset_generator.py

import json
import sqlite3
import hashlib
import argparse
from pathlib import Path
from datetime import datetime
from typing import Generator

CORRECTION_KEYWORDS = ["actually", "no, that", "wrong", "try again", "incorrect", "that's not"]
MIN_TOOL_CALLS_PER_EXAMPLE = 1

def generate_dataset(
    db_path: str,
    session_ids: list[str] | None,
    output_path: str,
    min_examples: int = 20,
) -> int:
    """
    Generate a fine-tuning JSONL dataset from successful agent interaction logs.

    Format: Each line is a JSON object with "messages" in OpenAI chat format.
    Filters out:
      - Turns with tool failures
      - Turns where user sent a correction after agent reply
      - Turns with fewer than MIN_TOOL_CALLS_PER_EXAMPLE tool calls

    Returns: Number of examples written.
    """
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Build session filter
    if session_ids:
        placeholders = ",".join("?" * len(session_ids))
        cursor.execute(
            f"SELECT DISTINCT session_id FROM messages WHERE session_id IN ({placeholders})",
            session_ids,
        )
    else:
        cursor.execute("SELECT DISTINCT session_id FROM messages WHERE requires_consolidation = 0")

    sessions = [row["session_id"] for row in cursor.fetchall()]

    output = Path(output_path)
    output.parent.mkdir(parents=True, exist_ok=True)

    examples_written = 0

    with output.open("w", encoding="utf-8") as f:
        for session_id in sessions:
            for example in extract_examples_from_session(conn, session_id):
                if example and quality_check(example):
                    f.write(json.dumps(example, ensure_ascii=False) + "\n")
                    examples_written += 1

    conn.close()

    print(f"✅ Dataset generated: {examples_written} examples → {output_path}")
    if examples_written < min_examples:
        print(f"⚠️  Warning: only {examples_written} examples (target: {min_examples}). "
              f"Run more sessions to build a larger dataset.")

    return examples_written


def extract_examples_from_session(conn: sqlite3.Connection, session_id: str) -> Generator:
    """
    Extract training examples from a single session's message log.
    Groups messages into (user_request, agent_response) pairs.
    """
    cursor = conn.cursor()
    cursor.execute(
        """
        SELECT role, content, tool_id, tool_status, timestamp
        FROM messages
        WHERE session_id = ?
        ORDER BY timestamp ASC
        """,
        (session_id,),
    )
    rows = cursor.fetchall()

    # Segment into turns: user → (assistant + tool_calls) → user → ...
    turns = []
    current_turn = []

    for row in rows:
        if row["role"] == "user" and current_turn:
            turns.append(current_turn)
            current_turn = []
        current_turn.append(dict(row))

    if current_turn:
        turns.append(current_turn)

    # Convert each turn group into a training example
    for i, turn_group in enumerate(turns[:-1]):  # Exclude last turn (no user follow-up to check)
        user_msg = next((m for m in turn_group if m["role"] == "user"), None)
        assistant_msg = next((m for m in turn_group if m["role"] == "assistant"), None)
        tool_calls = [m for m in turn_group if m["role"] == "tool_use"]
        tool_results = [m for m in turn_group if m["role"] == "tool_result"]

        if not user_msg or not assistant_msg:
            continue

        # Check next turn for user correction
        next_user = turns[i + 1][0] if i + 1 < len(turns) else None
        if next_user and any(kw in next_user["content"].lower() for kw in CORRECTION_KEYWORDS):
            continue  # Skip corrected turns

        # Check for tool failures
        has_failure = any(t["tool_status"] == "failure" for t in tool_results)
        if has_failure:
            continue

        # Build messages list
        messages = [{"role": "user", "content": user_msg["content"]}]

        # Include tool calls and results for multi-turn training
        for tc, tr in zip(tool_calls, tool_results):
            messages.append({
                "role": "assistant",
                "content": None,
                "tool_calls": [{
                    "type": "function",
                    "function": {"name": tc["tool_id"], "arguments": tc.get("content", "{}")}
                }]
            })
            messages.append({
                "role": "tool",
                "content": tr["content"]
            })

        messages.append({"role": "assistant", "content": assistant_msg["content"]})

        yield {"messages": messages, "session_id": session_id}


def quality_check(example: dict) -> bool:
    """Filter low-quality training examples."""
    messages = example["messages"]

    # Must have at least one tool call
    tool_calls = [m for m in messages if m.get("role") == "tool"]
    if len(tool_calls) < MIN_TOOL_CALLS_PER_EXAMPLE:
        return False

    # Assistant response must be substantive
    final_response = next((m for m in reversed(messages) if m.get("role") == "assistant"), None)
    if not final_response or not final_response.get("content"):
        return False

    if len(final_response["content"].split()) < 10:
        return False  # Too short

    # Check for secret taint contamination (basic heuristic)
    content = json.dumps(example)
    if any(pattern in content for pattern in ["sk-or-v1-", "ghp_", "Bearer ", "API_KEY"]):
        print(f"⚠️  Skipping example from session {example.get('session_id')} — possible credential leak")
        return False

    return True


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Generate fine-tuning dataset from Hydragent logs")
    parser.add_argument("--db", required=True, help="Path to sessions SQLite database")
    parser.add_argument("--sessions", nargs="*", help="Specific session IDs (omit for all)")
    parser.add_argument("--output", default="data/finetune/dataset.jsonl", help="Output JSONL path")
    parser.add_argument("--min-examples", type=int, default=20)
    args = parser.parse_args()

    generate_dataset(args.db, args.sessions, args.output, args.min_examples)
```

---

### 5.6 LoRA Fine-Tuning Pipeline

```python
# tools/finetune/lora_trainer.py

import yaml
import json
import torch
import argparse
from pathlib import Path
from datetime import datetime
from datasets import load_dataset
from transformers import AutoModelForCausalLM, AutoTokenizer, TrainingArguments, BitsAndBytesConfig
from peft import LoraConfig, get_peft_model, TaskType
from trl import SFTTrainer, DataCollatorForCompletionOnlyLM

def load_config(config_path: str) -> dict:
    with open(config_path) as f:
        return yaml.safe_load(f)

def run_training(
    dataset_path: str,
    lora_config_path: str = "tools/finetune/config/lora_config.yaml",
    training_config_path: str = "tools/finetune/config/training_config.yaml",
    output_dir: str | None = None,
    resume_from: str | None = None,
):
    lora_cfg = load_config(lora_config_path)
    train_cfg = load_config(training_config_path)

    run_id = datetime.utcnow().strftime("%Y%m%d_%H%M%S")
    output_dir = output_dir or f"data/models/lora_adapters/{run_id}"
    Path(output_dir).mkdir(parents=True, exist_ok=True)

    base_model_id = train_cfg["base_model"]
    print(f"🚀 LoRA fine-tuning: {base_model_id}")
    print(f"   Dataset: {dataset_path}")
    print(f"   Output:  {output_dir}")

    # Load dataset
    dataset = load_dataset("json", data_files=dataset_path, split="train")
    print(f"   Examples: {len(dataset)}")

    # 4-bit quantization config (QLoRA)
    bnb_config = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_use_double_quant=True,
        bnb_4bit_quant_type="nf4",
        bnb_4bit_compute_dtype=torch.bfloat16,
    )

    # Load base model
    model = AutoModelForCausalLM.from_pretrained(
        base_model_id,
        quantization_config=bnb_config,
        device_map="auto",
        trust_remote_code=True,
    )
    model.config.use_cache = False

    tokenizer = AutoTokenizer.from_pretrained(base_model_id, trust_remote_code=True)
    tokenizer.pad_token = tokenizer.eos_token
    tokenizer.padding_side = "right"

    # LoRA configuration
    lora_config = LoraConfig(
        r=lora_cfg.get("rank", 16),
        lora_alpha=lora_cfg.get("alpha", 32),
        target_modules=lora_cfg.get("target_modules", ["q_proj", "v_proj"]),
        lora_dropout=lora_cfg.get("dropout", 0.05),
        bias=lora_cfg.get("bias", "none"),
        task_type=TaskType.CAUSAL_LM,
    )

    model = get_peft_model(model, lora_config)
    model.print_trainable_parameters()

    # Training arguments
    training_args = TrainingArguments(
        output_dir=output_dir,
        num_train_epochs=train_cfg.get("epochs", 1),
        per_device_train_batch_size=train_cfg.get("batch_size", 2),
        gradient_accumulation_steps=train_cfg.get("grad_accum_steps", 4),
        learning_rate=train_cfg.get("learning_rate", 2e-4),
        warmup_ratio=train_cfg.get("warmup_ratio", 0.03),
        lr_scheduler_type="cosine",
        logging_steps=10,
        save_steps=train_cfg.get("save_steps", 100),
        save_total_limit=3,
        fp16=not torch.cuda.is_bf16_supported(),
        bf16=torch.cuda.is_bf16_supported(),
        report_to="none",
        resume_from_checkpoint=resume_from,
    )

    def format_messages(sample):
        """Format messages into a single training string."""
        text = tokenizer.apply_chat_template(
            sample["messages"],
            tokenize=False,
            add_generation_prompt=False,
        )
        return {"text": text}

    formatted_dataset = dataset.map(format_messages, remove_columns=dataset.column_names)

    trainer = SFTTrainer(
        model=model,
        tokenizer=tokenizer,
        train_dataset=formatted_dataset,
        dataset_text_field="text",
        max_seq_length=train_cfg.get("max_seq_length", 2048),
        args=training_args,
        packing=False,
    )

    print("🏋️  Training started...")
    trainer.train()

    # Save adapter
    adapter_path = Path(output_dir) / "final_adapter"
    trainer.model.save_pretrained(adapter_path)
    tokenizer.save_pretrained(adapter_path)

    # Save training metadata
    metadata = {
        "run_id": run_id,
        "base_model": base_model_id,
        "dataset_path": dataset_path,
        "dataset_size": len(dataset),
        "lora_config": lora_cfg,
        "training_config": train_cfg,
        "adapter_path": str(adapter_path),
        "completed_at": datetime.utcnow().isoformat(),
    }
    with open(Path(output_dir) / "metadata.json", "w") as mf:
        json.dump(metadata, mf, indent=2)

    print(f"✅ Training complete. Adapter saved to: {adapter_path}")
    return str(adapter_path)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--lora-config", default="tools/finetune/config/lora_config.yaml")
    parser.add_argument("--training-config", default="tools/finetune/config/training_config.yaml")
    parser.add_argument("--output", default=None)
    parser.add_argument("--resume", default=None, help="Resume from checkpoint path")
    args = parser.parse_args()

    run_training(args.dataset, args.lora_config, args.training_config, args.output, args.resume)
```

**`tools/finetune/config/lora_config.yaml`**:

```yaml
# LoRA hyperparameters
rank: 16                 # r — intrinsic rank of LoRA matrices. Higher = more capacity, more params
alpha: 32                # Scaling factor. Effective lr_scale = alpha / rank = 2.0
dropout: 0.05            # Dropout on LoRA layers
bias: "none"             # Options: none | all | lora_only
target_modules:
  - "q_proj"             # Query projection (standard for most BERT-family and LLaMA-family models)
  - "v_proj"             # Value projection
  # Optionally add: "k_proj", "o_proj", "gate_proj", "up_proj", "down_proj"
```

**`tools/finetune/config/training_config.yaml`**:

```yaml
# Base model (HuggingFace model ID)
base_model: "google/gemma-2-2b-it"   # 2B params — runs on 8GB VRAM in 4-bit

# Training parameters
epochs: 1
batch_size: 2                         # Per-device; effective batch = batch_size × grad_accum_steps
grad_accum_steps: 4                   # Effective batch size = 2 × 4 = 8
learning_rate: 2.0e-4
warmup_ratio: 0.03
max_seq_length: 2048
save_steps: 100
```

---

### 5.7 Benchmarking Harness & SKILL-BENCH

```rust
// crates/hydragent-bench/src/harness.rs

use crate::metrics::{BenchmarkMetrics, TaskResult};
use hydragent_types::BenchTask;
use sqlx::SqlitePool;

pub struct BenchmarkHarness {
    tasks: Vec<BenchTask>,
    orchestrator: Arc<dyn AgentOrchestrator>,
    db: SqlitePool,
}

impl BenchmarkHarness {
    pub async fn load_tasks(task_file: &str) -> anyhow::Result<Vec<BenchTask>> {
        let content = tokio::fs::read_to_string(task_file).await?;
        let tasks: Vec<BenchTask> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    /// Run all SKILL-BENCH tasks and return aggregate metrics.
    pub async fn run(&self) -> anyhow::Result<BenchmarkMetrics> {
        let start = std::time::Instant::now();
        let mut results = Vec::new();
        let mut total_score = 0.0f64;
        let mut total_cost = 0.0f64;

        tracing::info!(task_count = self.tasks.len(), "SKILL-BENCH started");

        for task in &self.tasks {
            let task_start = std::time::Instant::now();
            let response = self.orchestrator.process_single_turn(&task.input).await;
            let task_ms = task_start.elapsed().as_millis() as u64;

            let (score, reason) = match &response {
                Ok(resp) => self.score_response(&task, &resp.content),
                Err(e) => (0.0, format!("Error: {}", e)),
            };

            let response_text = response.map(|r| r.content).unwrap_or_default();
            total_score += score;
            total_cost += task.estimated_cost_usd.unwrap_or(0.001);

            tracing::debug!(
                task_id = %task.id,
                category = %task.category,
                score,
                reason = %reason,
                execution_ms = task_ms,
                "SKILL-BENCH task scored"
            );

            results.push(TaskResult {
                task_id: task.id.clone(),
                category: task.category.clone(),
                score,
                reason,
                response: response_text,
                execution_ms: task_ms,
            });
        }

        let total_tasks = self.tasks.len() as f64;
        let normalized_score = (total_score / total_tasks) * 100.0;
        let total_ms = start.elapsed().as_millis() as u64;

        tracing::info!(
            score = normalized_score,
            total_tasks = self.tasks.len(),
            total_ms,
            "SKILL-BENCH complete"
        );

        Ok(BenchmarkMetrics {
            score: normalized_score,
            total_tasks: self.tasks.len(),
            task_results: results,
            total_execution_ms: total_ms,
            total_cost_usd: total_cost,
            timestamp: chrono::Utc::now().timestamp_millis(),
        })
    }

    fn score_response(&self, task: &BenchTask, response: &str) -> (f64, String) {
        match task.scoring_method.as_str() {
            "exact_match" => {
                let expected = task.expected_answer.as_deref().unwrap_or("");
                if response.to_lowercase().contains(&expected.to_lowercase()) {
                    (1.0, "Exact match found".to_string())
                } else {
                    (0.0, format!("Expected '{}' not found in response", expected))
                }
            }

            "contains_all" => {
                let required: Vec<&str> = task.required_facts.iter().map(|s| s.as_str()).collect();
                let found: Vec<&str> = required.iter()
                    .filter(|&&fact| response.to_lowercase().contains(&fact.to_lowercase()))
                    .copied()
                    .collect();
                let score = found.len() as f64 / required.len() as f64;
                (score, format!("{}/{} required facts found", found.len(), required.len()))
            }

            "partial" => {
                // 0.5 for correct approach; 1.0 for correct final answer
                let has_correct_tool = task.required_tools.iter()
                    .all(|t| response.contains(t));
                let has_correct_answer = task.expected_answer.as_deref()
                    .map(|a| response.to_lowercase().contains(&a.to_lowercase()))
                    .unwrap_or(false);

                if has_correct_answer { (1.0, "Correct answer and approach".to_string()) }
                else if has_correct_tool { (0.5, "Correct approach, wrong answer".to_string()) }
                else { (0.0, "Incorrect approach and answer".to_string()) }
            }

            unknown => {
                tracing::warn!(method = unknown, "Unknown scoring method; defaulting to 0");
                (0.0, format!("Unknown scoring method: {}", unknown))
            }
        }
    }
}
```

---

### 5.8 Golden-Set Evaluator

```rust
// crates/hydragent-bench/src/golden_set.rs

use std::sync::Arc;

#[derive(Debug, serde::Deserialize)]
pub struct GoldenExample {
    pub id: String,
    pub question: String,
    pub expected_answer: String,
    pub answer_type: GoldenAnswerType,
    pub category: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoldenAnswerType {
    ExactMatch,              // Response must contain expected_answer verbatim
    NumericTolerance(f64),   // Response numeric value within ±tolerance% of expected
    SemanticSimilarity(f64), // Cosine similarity ≥ threshold (uses Phase 2 embedder)
}

pub struct GoldenSetEvaluator {
    examples: Vec<GoldenExample>,
    orchestrator: Arc<dyn AgentOrchestrator>,
    embedder: Option<Arc<LocalEmbedder>>,   // For SemanticSimilarity scoring
}

impl GoldenSetEvaluator {
    pub async fn load(path: &str) -> anyhow::Result<Vec<GoldenExample>> {
        let content = tokio::fs::read_to_string(path).await?;
        content.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<GoldenExample>(l))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("Failed to parse golden set: {}", e))
    }

    pub async fn evaluate(&self) -> anyhow::Result<GoldenSetReport> {
        let mut passed = 0usize;
        let mut failed = 0usize;
        let mut failures: Vec<GoldenFailure> = Vec::new();

        for example in &self.examples {
            let response = match self.orchestrator.process_single_turn(&example.question).await {
                Ok(r) => r.content,
                Err(e) => {
                    failed += 1;
                    failures.push(GoldenFailure {
                        id: example.id.clone(),
                        question: example.question.clone(),
                        expected: example.expected_answer.clone(),
                        actual: format!("ERROR: {}", e),
                        score: 0.0,
                    });
                    continue;
                }
            };

            let score = self.score(&example.answer_type, &response, &example.expected_answer).await;

            if score >= 0.5 {
                passed += 1;
            } else {
                failed += 1;
                failures.push(GoldenFailure {
                    id: example.id.clone(),
                    question: example.question.clone(),
                    expected: example.expected_answer.clone(),
                    actual: response,
                    score,
                });
            }
        }

        let total = self.examples.len();
        let pass_rate = if total > 0 { passed as f64 / total as f64 * 100.0 } else { 0.0 };

        tracing::info!(
            passed,
            failed,
            pass_rate = format!("{:.1}%", pass_rate),
            "Golden-set evaluation complete"
        );

        Ok(GoldenSetReport { passed, failed, pass_rate, failures })
    }

    async fn score(&self, answer_type: &GoldenAnswerType, response: &str, expected: &str) -> f64 {
        match answer_type {
            GoldenAnswerType::ExactMatch => {
                if response.to_lowercase().contains(&expected.to_lowercase()) { 1.0 } else { 0.0 }
            }

            GoldenAnswerType::NumericTolerance(tolerance) => {
                // Extract first number from response and compare to expected
                let resp_num = extract_first_number(response);
                let exp_num = extract_first_number(expected);
                match (resp_num, exp_num) {
                    (Some(r), Some(e)) if e != 0.0 => {
                        let pct_diff = ((r - e) / e).abs();
                        if pct_diff <= *tolerance { 1.0 } else { 0.0 }
                    }
                    _ => 0.0,
                }
            }

            GoldenAnswerType::SemanticSimilarity(threshold) => {
                if let Some(embedder) = &self.embedder {
                    let emb = embedder.clone();
                    let resp = response.to_string();
                    let exp = expected.to_string();
                    let result = tokio::task::spawn_blocking(move || {
                        let e1 = emb.embed_text(&resp)?;
                        let e2 = emb.embed_text(&exp)?;
                        Ok::<f64, anyhow::Error>(cosine_similarity(&e1, &e2) as f64)
                    }).await;

                    if let Ok(Ok(sim)) = result {
                        if sim >= *threshold { sim } else { 0.0 }
                    } else { 0.0 }
                } else {
                    // Fallback to exact match if embedder not available
                    if response.to_lowercase().contains(&expected.to_lowercase()) { 1.0 } else { 0.0 }
                }
            }
        }
    }
}

fn extract_first_number(s: &str) -> Option<f64> {
    regex::Regex::new(r"-?\d+(\.\d+)?").ok()
        .and_then(|re| re.find(s))
        .and_then(|m| m.as_str().parse().ok())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot / (norm_a * norm_b) }
}
```

---

### 5.9 Skill Composition & Chaining

```rust
// crates/hydragent-skills/src/composer.rs

use crate::library::SkillLibrary;
use crate::executor::{SkillExecutor, SkillExecutionResult};
use crate::skill::Skill;
use std::collections::HashMap;

/// Chains multiple skills sequentially, passing outputs as inputs.
///
/// Composition spec:
///   skill_a → output mapped to {{a_result}} → skill_b → output → skill_c
///
/// Mapping rules:
///   `output_map`: key = param name for next skill; value = path into previous result
pub struct SkillComposer {
    library: Arc<SkillLibrary>,
    executor: Arc<SkillExecutor>,
}

#[derive(Debug, serde::Deserialize)]
pub struct CompositionSpec {
    pub name: String,
    pub steps: Vec<CompositionStep>,
}

#[derive(Debug, serde::Deserialize)]
pub struct CompositionStep {
    pub skill_id: String,
    pub params: HashMap<String, String>,       // Can reference {{prev_output}} etc.
    pub output_binding: Option<String>,        // Bind this step's output to a variable name
}

impl SkillComposer {
    /// Execute a multi-skill composition.
    pub async fn execute_composition(
        &self,
        spec: &CompositionSpec,
        initial_input: &str,
    ) -> anyhow::Result<String> {
        let mut context: HashMap<String, String> = HashMap::new();
        context.insert("input".to_string(), initial_input.to_string());

        let mut last_output = initial_input.to_string();

        tracing::info!(
            composition = %spec.name,
            steps = spec.steps.len(),
            "Skill composition started"
        );

        for (i, step) in spec.steps.iter().enumerate() {
            let skill = self.library.get_skill(&step.skill_id).await?
                .ok_or_else(|| anyhow::anyhow!("Skill '{}' not found in library", step.skill_id))?;

            // Resolve params: substitute {{prev_output}} and other context vars
            let resolved_params: HashMap<String, String> = step.params.iter()
                .map(|(k, v)| {
                    let resolved = resolve_context_var(v, &context, &last_output);
                    (k.clone(), resolved)
                })
                .collect();

            let result = self.executor.execute(&skill, resolved_params, &last_output).await?;

            if !result.success {
                anyhow::bail!(
                    "Composition '{}' failed at step {} ({}: {})",
                    spec.name, i + 1, step.skill_id,
                    result.error.unwrap_or_default()
                );
            }

            last_output = result.output.clone();

            // Bind output to context variable if specified
            if let Some(binding) = &step.output_binding {
                context.insert(binding.clone(), result.output.clone());
            }

            tracing::debug!(
                step = i + 1,
                skill = %step.skill_id,
                output_len = result.output.len(),
                "Composition step complete"
            );
        }

        tracing::info!(
            composition = %spec.name,
            "Skill composition complete"
        );

        Ok(last_output)
    }
}

fn resolve_context_var(template: &str, context: &HashMap<String, String>, last_output: &str) -> String {
    let mut result = template.to_string();
    result = result.replace("{{prev_output}}", last_output);
    result = result.replace("{{input}}", context.get("input").map(String::as_str).unwrap_or(""));
    for (k, v) in context {
        result = result.replace(&format!("{{{{{}}}}}", k), v);
    }
    result
}
```

---

### 5.10 Skill Observability Dashboard

```python
# adapters/skill_dashboard.py

from rich.console import Console
from rich.table import Table
from rich.panel import Panel
from rich.columns import Columns
from rich import box
import sqlite3
import argparse

def render_skill_dashboard(db_path: str):
    """Render a rich terminal dashboard of skill library stats."""
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    console = Console()

    # Summary stats
    cursor = conn.cursor()
    cursor.execute("""
        SELECT tier, COUNT(*) as count, AVG(success_rate) as avg_rate
        FROM skills
        GROUP BY tier
        ORDER BY CASE tier
            WHEN 'active' THEN 1
            WHEN 'candidate' THEN 2
            WHEN 'inactive' THEN 3
            WHEN 'archived' THEN 4
        END
    """)
    tier_stats = cursor.fetchall()

    summary_lines = []
    tier_colors = {"active": "green", "candidate": "yellow", "inactive": "red", "archived": "dim"}
    for row in tier_stats:
        color = tier_colors.get(row["tier"], "white")
        summary_lines.append(
            f"[{color}]{row['tier'].capitalize()}[/{color}]: {row['count']} skills "
            f"(avg success: {row['avg_rate']:.0%})"
        )

    console.print(Panel("\n".join(summary_lines), title="🧠 Skill Library Overview", border_style="blue"))

    # Top skills table
    cursor.execute("""
        SELECT s.name, s.tier, s.success_rate, s.execution_count,
               GROUP_CONCAT(st.tag, ', ') as tags
        FROM skills s
        LEFT JOIN skill_tags st ON s.id = st.skill_id
        WHERE s.tier IN ('active', 'candidate')
        GROUP BY s.id
        ORDER BY s.success_rate DESC, s.execution_count DESC
        LIMIT 20
    """)
    skills = cursor.fetchall()

    table = Table(title="Top Skills", box=box.ROUNDED, expand=True)
    table.add_column("Name", style="bold cyan")
    table.add_column("Tier")
    table.add_column("Success Rate", justify="right")
    table.add_column("Executions", justify="right")
    table.add_column("Tags", style="dim")

    tier_style = {"active": "green", "candidate": "yellow", "inactive": "red", "archived": "dim"}
    for row in skills:
        style = tier_style.get(row["tier"], "white")
        table.add_row(
            row["name"],
            f"[{style}]{row['tier']}[/{style}]",
            f"{row['success_rate']:.0%}",
            str(row["execution_count"]),
            row["tags"] or "—",
        )

    console.print(table)

    # Recent skill executions
    cursor.execute("""
        SELECT s.name, se.success, se.execution_ms, datetime(se.executed_at/1000, 'unixepoch') as when_str
        FROM skill_executions se
        JOIN skills s ON s.id = se.skill_id
        ORDER BY se.executed_at DESC
        LIMIT 10
    """)
    recent = cursor.fetchall()

    exec_table = Table(title="Recent Executions (last 10)", box=box.SIMPLE)
    exec_table.add_column("Skill")
    exec_table.add_column("Result")
    exec_table.add_column("Time (ms)", justify="right")
    exec_table.add_column("When")

    for row in recent:
        result_str = "[green]✅ Success[/green]" if row["success"] else "[red]❌ Failed[/red]"
        exec_table.add_row(row["name"], result_str, str(row["execution_ms"]), row["when_str"])

    console.print(exec_table)
    conn.close()

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--db", default="data/sessions/hydragent.db")
    args = parser.parse_args()
    render_skill_dashboard(args.db)
```

---

## 6. Built-in Tools (Phase 7 Additions)

### `skill_store`

```yaml
name: skill_store
description: "Save a reusable procedure as a named skill in the skill library. Use when you successfully complete a multi-step task that could be useful in the future."
tier: auto_approve
params_schema:
  type: object
  required: [name, description, steps]
  properties:
    name:
      type: string
      description: "Short kebab-case skill name (e.g., 'convert-csv-to-json')"
    description:
      type: string
      description: "One sentence describing what this skill does"
    tags:
      type: array
      items: { type: string }
      description: "Capability tags for skill discovery"
      default: []
    steps:
      type: array
      description: "The skill trajectory steps"

output:
  type: object
  properties:
    skill_id: { type: string }
    name:     { type: string }
    stored:   { type: boolean }
```

---

### `skill_run`

```yaml
name: skill_run
description: "Execute a named skill from the library with specific parameters. Use instead of a full ReAct loop when you know a suitable skill exists."
tier: auto_approve
params_schema:
  type: object
  required: [skill_name_or_id]
  properties:
    skill_name_or_id:
      type: string
      description: "Name or UUID of the skill to run"
    params:
      type: object
      description: "Parameter bindings for the skill's {{param}} placeholders"
      additionalProperties: { type: string }
    input:
      type: string
      description: "Primary input for the skill"
      default: ""

output:
  type: object
  properties:
    success:       { type: boolean }
    output:        { type: string }
    skill_name:    { type: string }
    execution_ms:  { type: integer }
```

---

### `skill_search`

```yaml
name: skill_search
description: "Search the skill library by keyword or natural language description. Use before running a complex task to check if a suitable skill already exists."
tier: auto_approve
params_schema:
  type: object
  required: [query]
  properties:
    query:
      type: string
      description: "Natural language description of the task you want to perform"
    limit:
      type: integer
      default: 5
      minimum: 1
      maximum: 20

output:
  type: object
  properties:
    results:
      type: array
      items:
        type: object
        properties:
          skill_id:     { type: string }
          name:         { type: string }
          description:  { type: string }
          success_rate: { type: number }
          tier:         { type: string }
```

---

## 7. Configuration & Environment

### Updated `.env.example`

```ini
# ── Phase 1-6 (unchanged) ─────────────────────────────────────────────────
OPENROUTER_API_KEYS=sk-or-v1-...
ENABLE_SWARM=true
ENABLE_SECURITY_PIPELINE=true

# ── Phase 7: Skill Engine ─────────────────────────────────────────────────

# Enable skill lookup on every user turn
ENABLE_SKILL_ENGINE=true

# Minimum similarity score to prefer a skill over full ReAct
SKILL_MATCH_THRESHOLD=0.85

# Enable automatic skill extraction from successful trajectories
ENABLE_SKILL_INDUCTION=true

# Directory for skill YAML files
SKILLS_DIR=./skills

# ── Phase 7: 7-Day Curator ────────────────────────────────────────────────

# Enable weekly skill curation (runs Sunday at 3 AM)
ENABLE_SKILL_CURATOR=true

# Curator promotion/demotion thresholds
CURATOR_PROMOTE_THRESHOLD=0.80
CURATOR_DEMOTE_THRESHOLD=0.50
CURATOR_MIN_EXECUTIONS=5
CURATOR_ARCHIVE_DAYS=30

# Channel for curator digest notifications
CURATOR_NOTIFY_CHANNEL=telegram:123456789

# ── Phase 7: LoRA Fine-Tuning ─────────────────────────────────────────────

# Enable fine-tuning pipeline (requires GPU)
ENABLE_FINETUNE=false

# Directory for LoRA adapter outputs
LORA_OUTPUT_DIR=./data/models/lora_adapters

# After training, use the LoRA adapter for new sessions
LORA_ADAPTER_PATH=./data/models/lora_adapters/latest/final_adapter

# ── Phase 7: Benchmarking ─────────────────────────────────────────────────

# SKILL-BENCH task file
BENCH_TASKS_PATH=./bench/skill_bench_tasks.jsonl

# Golden-set evaluation file
BENCH_GOLDEN_SET_PATH=./bench/golden_set.jsonl

# Benchmark results output directory
BENCH_RESULTS_DIR=./bench/results

# Benchmark history SQLite database
BENCH_HISTORY_DB=./data/bench_history.db
```

---

## 8. Testing Strategy

### 8.1 Unit Tests

| Test File | What It Covers |
|---|---|
| `skill_extractor_test.rs` | Eligible trajectory (2+ tools, no corrections) → extracts skill; ineligible (1 tool) → `None`; failed tools → `None`; extracted params contain `{{file_path}}` for path-like strings |
| `skill_executor_test.rs` | Correct param substitution `{{query}}` → "actual value"; missing required param → `Err(MissingParam)`; tool failure → `SkillExecutionResult { success: false }`; multi-step execution completes in < 100ms for EchoTool |
| `curator_test.rs` | success_rate=0.9 candidate → promoted to Active; success_rate=0.3, executions=10 → demoted to Inactive; last_executed=45 days ago → archived; < 5 executions → stays Candidate |
| `golden_set_test.rs` | ExactMatch: response contains expected → 1.0; does not → 0.0; NumericTolerance(0.05): 100 vs 102 → 1.0; 100 vs 120 → 0.0; SemanticSimilarity: similar sentences → ≥ 0.8 |
| `composer_test.rs` | 3-step chain passes output correctly; failure at step 2 → `Err`; `{{prev_output}}` in params resolved from previous step's output |

### 8.2 Integration Tests

```rust
// tests/integration/skill_induction_test.rs

#[tokio::test]
async fn test_skill_induced_from_successful_trajectory() {
    let db = setup_test_db().await;
    let llm = MockLLM::returns(r#"{
        "extractable": true,
        "name": "search-and-summarize",
        "description": "Searches the web for a query and summarizes the results",
        "capability_tags": ["research", "summarization"],
        "params": [
            {"name": "query", "type": "string", "description": "Search query", "required": true}
        ],
        "steps": [
            {"role": "thought", "content": "I need to search for {{query}}"},
            {"role": "action", "tool": "web_search", "params_template": {"query": "{{query}}"}},
            {"role": "response", "template": "Here is a summary about {{query}}: {{web_search_result}}"}
        ]
    }"#);

    let extractor = SkillExtractor::new(Arc::new(llm));

    let turns = vec![
        ConversationTurn { role: "user".into(), content: "Search for Rust async features".into(), tool_id: None, tool_status: None },
        ConversationTurn { role: "thought".into(), content: "I'll use web_search".into(), tool_id: None, tool_status: None },
        ConversationTurn { role: "tool_use".into(), content: r#"{"query": "Rust async features"}"#.into(), tool_id: Some("web_search".into()), tool_status: Some("success".into()) },
        ConversationTurn { role: "tool_result".into(), content: "Rust async uses tokio...".into(), tool_id: Some("web_search".into()), tool_status: Some("success".into()) },
        ConversationTurn { role: "tool_use".into(), content: r#"{"text": "..."}"#.into(), tool_id: Some("memory_store".into()), tool_status: Some("success".into()) },
        ConversationTurn { role: "tool_result".into(), content: "Stored.".into(), tool_id: Some("memory_store".into()), tool_status: Some("success".into()) },
        ConversationTurn { role: "assistant".into(), content: "Here is a summary about Rust async...".into(), tool_id: None, tool_status: None },
    ];

    let candidate = extractor.extract_from_trajectory(&turns, "test-session").await.unwrap();
    assert!(candidate.is_some());
    let skill = candidate.unwrap();
    assert!(skill.extractable);
    assert_eq!(skill.name.as_deref(), Some("search-and-summarize"));
    assert!(skill.params.as_ref().unwrap().iter().any(|p| p.name == "query"));
}

#[tokio::test]
async fn test_skill_executor_replays_correctly() {
    let tools = create_test_tool_registry(); // EchoTool registered
    let executor = SkillExecutor::new(Arc::new(tools));

    let skill = Skill {
        id: "test-skill-id".into(),
        name: "echo-skill".into(),
        params: vec![
            SkillParam { name: "message".into(), required: true, .. Default::default() }
        ],
        trajectory: vec![
            SkillStep { role: "action".into(), tool: Some("echo".into()),
                params_template: Some(serde_json::json!({"text": "{{message}}"})),
                content: String::new(), template: None },
            SkillStep { role: "response".into(), template: Some("Echo result: {{echo_result}}".into()),
                content: String::new(), tool: None, params_template: None },
        ],
        ..Default::default()
    };

    let mut params = std::collections::HashMap::new();
    params.insert("message".to_string(), "Hello from test".to_string());

    let result = executor.execute(&skill, params, "Hello").await.unwrap();
    assert!(result.success);
    assert!(result.output.contains("Echo result"));
}
```

```python
# tests/integration/lora_dataset_test.py

import json
import pytest
from pathlib import Path
import tempfile

def test_dataset_generator_output():
    """Test that dataset_generator produces valid JSONL with correct schema."""
    from tools.finetune.dataset_generator import generate_dataset

    with tempfile.NamedTemporaryFile(suffix=".jsonl", mode="w", delete=False) as f:
        output_path = f.name

    # Use test fixture database
    count = generate_dataset(
        db_path="tests/fixtures/test_sessions.db",
        session_ids=None,
        output_path=output_path,
        min_examples=0,
    )

    assert count >= 0, "generate_dataset should not raise"

    # Validate each line is valid JSON with correct schema
    with open(output_path) as f:
        for i, line in enumerate(f):
            example = json.loads(line)
            assert "messages" in example, f"Line {i}: missing 'messages' key"
            assert isinstance(example["messages"], list), f"Line {i}: messages must be list"
            assert len(example["messages"]) >= 2, f"Line {i}: need at least user + assistant"
            assert example["messages"][0]["role"] == "user"
            assert example["messages"][-1]["role"] == "assistant"

def test_no_credentials_in_dataset():
    """Ensure quality_check blocks examples containing credential patterns."""
    from tools.finetune.dataset_generator import quality_check

    dirty_example = {
        "messages": [
            {"role": "user", "content": "What is my API key?"},
            {"role": "assistant", "content": "Your key is sk-or-v1-abc123..."},
        ],
        "session_id": "test",
    }
    assert not quality_check(dirty_example), "Credential-containing example should be blocked"
```

### 8.3 Manual QA Checklist (Phase 7 Sign-off)

```
[ ] Have a 10+ turn session with multiple successful tool uses
    → Check skill library after Dreaming cycle: `./hydragent skills list` → new skills visible
[ ] Ask a question matching an induced skill → agent uses skill (check tracing: "Skill match found")
    vs full ReAct (no skill log line)
[ ] Manually check that skill params are correctly substituted:
    skill "search-and-summarize" with {{query}} → verify "Rust book" is substituted for "{{query}}"
[ ] Wait for 7-Day Curator to run (or trigger manually via CLI):
    → Check skill tiers changed correctly based on execution history
    → Telegram notification received with digest
[ ] Run dataset generator:
    `python tools/finetune/dataset_generator.py --db data/sessions/hydragent.db --output /tmp/test.jsonl`
    → Verify output is valid JSONL; no credential-like strings; all examples have tool calls
[ ] Run LoRA training with --dry-run flag (mock training):
    `python tools/finetune/lora_trainer.py --dataset /tmp/test.jsonl --dry-run`
    → No OOM; metadata.json written
[ ] Run SKILL-BENCH:
    `./hydragent bench run` → JSON report in bench/results/{version}/report.json
[ ] Run golden-set evaluator:
    `./hydragent bench golden-set` → pass rate ≥ 80%
[ ] `./hydragent skills list` → table with name, tier, success rate, tags
[ ] `./hydragent bench history` → shows current run score
[ ] `cargo test --workspace` → exits 0
[ ] `pytest adapters/ -v` → exits 0
```

---

## 9. Performance Targets

| Metric | Target | Measurement Method |
|---|---|---|
| Skill lookup (FTS5 + embedding) | < 20 ms | `tracing::instrument` span in `orchestrator::find_skill` |
| Skill execution (5-step EchoTool) | < 50 ms | Integration test timing |
| Skill extraction LLM call | < 10 s | Bound by LLM; triggered only during Dreaming (background) |
| Skill induction per Dreaming cycle | < 30 s total | Concurrent extraction with tokio::spawn |
| Dataset generation (1,000 turns) | < 5 s | SQLite query + JSONL write benchmark |
| LoRA training (100 examples, 1 epoch, Gemma-2-2B) | < 30 min | Measured on A100 in lora_trainer.py |
| SKILL-BENCH full run (80 tasks) | < 15 min | Each task: 1 LLM call; sequential for reproducibility |
| Golden-set evaluation (30 items) | < 5 min | 30 sequential LLM calls |
| Curator weekly cycle (100 skills) | < 10 s | SQLite aggregation + tier updates |
| Skill similarity dedup (1,000 skills) | < 5 s | Pairwise HNSW search; done during Dreaming |

---

## 10. Risks & Mitigations

| Risk | Category | Likelihood | Impact | Mitigation Strategy |
|---|---|---|---|---|
| **Skill induction hallucination** (LLM invents params that don't exist) | LLM Quality | Medium | Medium | Validate every extracted param name against the actual trajectory text. If `{{param}}` appears in template but not in trajectory → reject extraction. |
| **LoRA catastrophic forgetting** | ML | Low | Medium | QLoRA (4-bit) inherently prevents full weight updates. LoRA adapters are additive. Verify baseline SKILL-BENCH score unchanged after fine-tuning. |
| **Dataset contamination** (Secret-tainted data in training set) | Security | Low | Critical | `quality_check()` in `dataset_generator.py` scans for credential patterns. Phase 6 taint tracker integration: exclude any messages with `TaintCategory::Secret` label from the export. |
| **Curator too aggressive** (demotes skills that are just unused, not failing) | Logic | Medium | Medium | `min_executions_for_demotion=5` ensures we have enough data before demotion. `archive_inactive_days=30` gives long tail of low-use skills time to recover. |
| **Skill match false positive** (wrong skill invoked) | Correctness | Medium | Medium | `SKILL_MATCH_THRESHOLD=0.85` is conservative. Below threshold, always fall back to full ReAct. Log all skill matches for user review via `./hydragent skills list --recent-matches`. |
| **LoRA training OOM on low VRAM** | Infrastructure | High | Low | 4-bit QLoRA on Gemma-2-2B requires only 6 GB VRAM. Document minimum GPU spec. `ENABLE_FINETUNE=false` by default — the agent works without it. |
| **SKILL-BENCH tasks becoming stale** | Evaluation | Medium | Low | Tasks are JSON; easy to update. Pin benchmark version in `bench/results/{version}/`. Compare score trends across versions, not just absolute scores. |
| **Skill name collision** (two induced skills with same name) | Storage | Low | Low | `skills.name` is `UNIQUE`; duplicate name triggers SQLite constraint error. `SkillLibrary::insert_skill()` catches this and appends a suffix (`-v2`, `-v3`). |

---

## 11. Definition of Done

Phase 7 is **complete** when all of the following are true:

### Code

- [ ] `cargo build --workspace` and `cargo test --workspace` exit 0 with `RUSTFLAGS="-D warnings"`
- [ ] `cargo clippy --workspace -- -D warnings` exits 0
- [ ] `pytest adapters/ -v` exits 0
- [ ] All Phase 1–6 tests remain green (zero regressions)

### Skill Engine

- [ ] 3 builtin skills load at startup from `skills/builtin/*.yaml`
- [ ] Skill extractor induces ≥ 1 skill from 20 successful turns in integration test
- [ ] Skill executor correctly substitutes `{{param}}` placeholders in integration test
- [ ] Skill lookup prefers skills over full ReAct when similarity > 0.85 (verified in integration test)

### 7-Day Curator

- [ ] Curation cycle runs without error on 100 synthetic skills
- [ ] Promotion/demotion decisions match expected outcomes in unit test
- [ ] Telegram digest delivered after curator run in integration test
- [ ] Zero regressions to scheduler (cron job still runs after Phase 7 changes)

### Benchmarking

- [ ] SKILL-BENCH produces a JSON report with 80 tasks scored
- [ ] Golden-set pass rate ≥ 80% (30 Q&A pairs)
- [ ] Benchmark history stored in SQLite and queryable via `./hydragent bench history`

### Fine-Tuning Pipeline

- [ ] `dataset_generator.py` produces valid JSONL (schema validated in integration test)
- [ ] No Secret-tainted data in generated dataset (quality_check test passes)
- [ ] `lora_trainer.py` completes at least 1 training step without error (mock mode)

### Documentation

- [ ] `ARCHITECTURE.md` updated with skill engine layer
- [ ] `tools/finetune/README.md` written: setup, GPU requirements, how to run
- [ ] `bench/README.md` written: SKILL-BENCH methodology, how to add new tasks
- [ ] `PHASE_7.md` (this file) reviewed and reflects actual implementation

### Release

- [ ] `v0.7.0` git tag created
- [ ] `CHANGELOG.md` entry written
- [ ] Baseline SKILL-BENCH score published in GitHub Release notes
- [ ] `bench/results/v0.7.0/report.json` committed to repository

---

*Previous phase: [PHASE_6.md](PHASE_6.md) — 16-Layer Security Pipeline (Weeks 23–26)*
*Next phase: [PHASE_8.md](PHASE_8.md) — Production Hardening: Binary Optimization, Telemetry & Enterprise Deployment (Weeks 31–34)*
