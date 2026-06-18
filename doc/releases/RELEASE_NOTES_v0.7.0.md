# Release Notes — v0.7.0 — Skill Engine & Curator

> **Tag**: `v0.7.0` · **Date**: 2026-06-14 · **Codename**: *Hermes*

Phase 7 ships the **self-improving skill engine**: a persistent skill library, a deterministic
Hermes-style skill inducer, an executor with prompt-rendering and tool-allowlist enforcement, a
7-day curator that promotes/demotes skills, a 80-task SKILL-BENCH + 30-pair golden-set retrieval
benchmark, and a Python LoRA fine-tuning pipeline.

---

## Highlights

### 🎯 Self-Improving Skill Engine (`hydragent-skills`)

- **SQLite-backed `SkillLibrary`** with FTS5 full-text search, tag retrieval, and version history
  ([library.rs](crates/hydragent-skills/src/library.rs))
- **`SkillExtractor`** (Hermes) — deterministic heuristic that turns a successful ReAct trajectory
  into a `SkillCandidate` with Mustache `{{param}}` placeholders
  ([extractor.rs](crates/hydragent-skills/src/extractor.rs))
- **`SkillExecutor`** — renders prompts, validates required params, enforces a tool allowlist,
  records `SkillExecutionRecord` rows for the curator
  ([executor.rs](crates/hydragent-skills/src/executor.rs))
- **`SevenDayCurator`** — promotes `Candidate` → `Active` (success_rate ≥ 0.7 over ≥ 10 runs),
  demotes / archives underperformers
  ([curator.rs](crates/hydragent-skills/src/curator.rs))
- **`SkillComposer`** — merges ≥ 2 compatible skills and resolves placeholder conflicts
  ([composer.rs](crates/hydragent-skills/src/composer.rs))
- **52 unit + integration tests** passing, including a 4-test end-to-end dreaming integration
  in [`hydragent-core/src/skill_induction.rs`](crates/hydragent-core/src/skill_induction.rs)

### 📚 3 Builtin Skills (`skills/builtin/*.yaml`)

- `convert-csv-to-json` — CSV string → JSON array (RFC 4180 escaping)
- `summarize-github-issue` — issue body + first 30 comments → TL;DR
- `debug-rust-error` — Rust compiler error → root cause + minimal repro

Seed them into `data/skill_library.sqlite` once with the
[`skills-import`](crates/hydragent-skills/bin/skills_import.rs) CLI
(`cargo run -p hydragent-skills --bin skills-import -- --verbose`).
The import is idempotent — re-running it is a no-op apart from a
`last_updated` bump — so it's safe to wire into a setup script.
`--dry-run` parses every YAML without touching the database (exit 2
on any malformed file, suitable for CI); `--list` dumps the current
library contents.

### 📊 SKILL-BENCH & Golden Set (`hydragent-bench`)

- **SKILL-BENCH v1** — 80 unique retrieval tasks across 10 skills × 8 paraphrases
  ([`tests/bench/skill_bench_v1.jsonl`](tests/bench/skill_bench_v1.jsonl))
- **Golden Set v1** — 30 hand-verified multi-relevance pairs (10 single / 15 dual / 5 triple)
  ([`tests/bench/golden_set_v1.jsonl`](tests/bench/golden_set_v1.jsonl))
- **`hydragent-bench` crate** — 25 unit + 5 integration tests
  ([`crates/hydragent-bench/`](crates/hydragent-bench))
- **CLI binary** `cargo run -p hydragent-bench --bin bench` produces a JSON `BenchReport` with
  Recall@1/3/5, MRR, mean Precision/Recall/F1

### 🧠 Python LoRA Fine-Tuning Pipeline (`tools/finetune/`)

- `generate_dataset.py` — extracts successful ReAct turns → JSONL training data
- `train_lora.py` — 4-bit `peft` + `bitsandbytes` LoRA trainer (Gemma 2 2B default)
- Quality filtering: only turns with ≥ 1 tool successfully used, no user-corrections
- Taint-checked: zero `Secret`-classified data leaks into training sets

### 🧪 Dreaming Integration

- `run_dream_cycle` now takes `Option<Arc<SkillLibrary>>`; after consolidation it runs
  `run_skill_induction` over the recent successful trajectories and dedupes via
  `SkillExtractor::is_duplicate` (Jaccard over capability_tags)
- Dreaming worker spawned in `hydragent-core/src/main.rs` with the library handle

---

## Upgrade Notes

- **No breaking API changes** to existing crates (`hydragent-model`, `hydragent-tools`, etc.)
- New `Skill`, `SkillTier`, `SkillParam`, `SkillVersion` types added to `hydragent-types` (purely
  additive)
- New tables: `skills`, `skill_versions`, `skill_tags`, `skill_executions`, `skills_fts`
  (migration [`005_skill_library.sql`](migrations/005_skill_library.sql))
- FTS5 virtual table uses an explicit `skill_id UNINDEXED` column to bridge TEXT-UUID primary
  keys; joins are `ON fts.skill_id = s.id`

## Test Count

| Crate               | Unit | Integration | Total |
| ------------------- | ---: | ----------: | ----: |
| `hydragent-skills`  |   48 |           4 |    52 |
| `hydragent-bench`   |   25 |           5 |    30 |
| `hydragent-core` (skill induction) | 4 | 0 | 4 |
| **Phase 7 net new** | **77** | **9** | **86** |

Pre-existing workspace tests: 542 passing. One pre-existing failure in
`hydragent-model --test custom_openai_integration` (requires live OpenAI endpoint, unrelated
to Phase 7).

## Reproducing the Benchmark

```bash
# Build & test
cargo test -p hydragent-bench --tests

# Run on real data
cargo run -p hydragent-bench --bin bench -- \
    --skill-bench tests/bench/skill_bench_v1.jsonl \
    --golden-set  tests/bench/golden_set_v1.jsonl \
    --output      reports/bench-v0.7.0.json
```

## What Comes Next (Phase 8)

- BenchRunner integration: wire a full ReAct agent to SKILL-BENCH and record baseline R@1
- Skill induction at scale: 1,000+ trajectories → 100+ candidate skills
- Adversarial eval: red-team skill induction with poisoned trajectories
