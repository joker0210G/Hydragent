# Changelog

All notable changes to hydragent are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0] — 2026-06-14

Phase 7: **Self-Improving Skill Engine & Curator**. Adds a persistent
SQLite-backed `SkillLibrary`, a Hermes-style deterministic skill
inducer, a `SkillExecutor` with prompt-rendering + tool-allowlist
enforcement, a 7-day curator that promotes/demotes skills, an
80-task SKILL-BENCH + 30-pair golden-set retrieval benchmark, and a
Python LoRA fine-tuning pipeline. See
[RELEASE_NOTES_v0.7.0.md](RELEASE_NOTES_v0.7.0.md) for the full
walk-through.

### Added

- **`hydragent-skills` crate** (new) — 48 unit + 4 integration tests
  - `SkillLibrary` with FTS5 full-text search and tag retrieval
    ([library.rs](crates/hydragent-skills/src/library.rs))
  - `SkillExtractor` (Hermes) — deterministic heuristic skill inducer
    ([extractor.rs](crates/hydragent-skills/src/extractor.rs))
  - `SkillExecutor` — Mustache prompt rendering + tool-allowlist
    enforcement + execution record
    ([executor.rs](crates/hydragent-skills/src/executor.rs))
  - `SevenDayCurator` — `Candidate` → `Active` (≥ 0.7 success over
    ≥ 10 runs); demote / archive underperformers
    ([curator.rs](crates/hydragent-skills/src/curator.rs))
  - `SkillComposer` — merge ≥ 2 compatible skills with conflict
    resolution ([composer.rs](crates/hydragent-skills/src/composer.rs))
  - `SkillParam`, `SkillVersion`, `SkillExecutionRecord` types in
    `hydragent-types` with 11 unit tests
- **`hydragent-bench` crate** (new) — 25 unit + 5 integration tests
  - `SkillBenchTask` + `GoldenSetItem` loaders
    ([dataset.rs](crates/hydragent-bench/src/dataset.rs))
  - `recall_at_k`, `reciprocal_rank`, `Prf` (precision/recall/F1)
    metrics ([metrics.rs](crates/hydragent-bench/src/metrics.rs))
  - `SkillBenchScores` + `GoldenScores` aggregators
    ([runner.rs](crates/hydragent-bench/src/runner.rs))
  - `BenchReport` JSON serialisation
    ([report.rs](crates/hydragent-bench/src/report.rs))
  - `bench` CLI binary (`cargo run -p hydragent-bench --bin bench`)
- **SKILL-BENCH v1** — 80 unique retrieval tasks across 10 skills
  ([skill_bench_v1.jsonl](tests/bench/skill_bench_v1.jsonl))
- **Golden Set v1** — 30 hand-verified multi-relevance pairs
  ([golden_set_v1.jsonl](tests/bench/golden_set_v1.jsonl))
- **3 builtin skills** shipped in
  [`skills/builtin/`](skills/builtin): `convert-csv-to-json`,
  `summarize-github-issue`, `debug-rust-error`
- **`skills-import` CLI** — `cargo run -p hydragent-skills --bin skills-import --`
  walks a directory of `*.yaml` skills and upserts them into the
  library. Idempotent (safe to re-run), supports `--dry-run` for CI
  validation, and `--list` for verifying what's in the library.
  Seeds the 3 builtins above into `data/skill_library.sqlite` on
  first boot. 5 unit tests.
- **Migration `005_skill_library.sql`** — 4 tables, FTS5 virtual
  table, 4 sync triggers
- **Python LoRA fine-tuning pipeline** in
  [`tools/finetune/`](tools/finetune): `generate_dataset.py`,
  `train_lora.py`, requirements + README
- **Dreaming integration** in
  [`hydragent-core/src/skill_induction.rs`](crates/hydragent-core/src/skill_induction.rs) —
  `run_dream_cycle` now extracts candidate skills after consolidation
  (4 new unit tests)
- **`SkillLibrary::open` wired into `hydragent-core::main.rs`** with an
  `Arc<SkillLibrary>` handle passed to the dream worker

### Test count

| Crate / Surface                          | Unit | Integration | Total |
|------------------------------------------|-----:|------------:|------:|
| `hydragent-skills`                       |   48 |           4 |    52 |
| `hydragent-bench`                        |   25 |           5 |    30 |
| `hydragent-core` (skill induction)       |    4 |           0 |     4 |
| **Phase 7 net new**                      | **77** | **9**    | **86** |
| Pre-existing workspace                   |    — |           — |   542 passing, 1 pre-existing failure (`custom_openai_integration`, requires live OpenAI endpoint, unrelated to Phase 7) |

## [0.6.1] — 2026-06-14

Phase 6 user-perspective bug-fix patch: closes 4 issues found during
`hydragent chat` CLI testing of the v0.6.0 release.

### Fixed

- **AskUser over-triggering** — the strategy router prompt now ships an
  explicit `AVAILABLE TOOLS` inventory and 3 routing rules. Rule R1
  forbids `ask_user` for project-specific concepts that have a tool
  (taint policy, vault status, sanitizer scan, etc.). Rule R2 routes
  known jailbreak templates to `react_loop` so the sanitizer can score
  them. Rule R3 raises the bar for `ask_user` to only when no tool
  can resolve the request.
- **Pending-clarification state bleed** — the orchestrator no longer
  auto-wraps the next user message as an answer to a pending
  clarification. A new `looks_like_clarification_answer` heuristic
  decides on a per-message basis: short replies free of question
  marks and action verbs are treated as answers; anything that looks
  like a fresh request is treated as a fresh request (with a status
  notice that the previous clarification was discarded). 5 unit
  tests cover the heuristic.
- **Sanitizer patterns not exposed to the LLM** — new
  `sanitizer_list_patterns` tool returns the full pattern inventory
  (id, severity, description, regex preview) so the LLM can look up
  the exact id of a pattern that fired in a `sanitizer_scan` result.
  No parameters; auto-approved.
- **Vault not initialized** — new `security vault-init` subcommand
  wraps the existing `Vault::init` call with an idempotent path and
  helpful next-step hints. The original `hydragent vault init` still
  works.

### Changed

- `crates/hydragent-core/src/strategy.rs` — strategy-router prompt
  rewritten (now ~800 tokens, was ~250).
- `crates/hydragent-core/src/orchestrator.rs` — `IntentSubmitHandler`
  pending-clarification block replaced with heuristic dispatch; new
  `looks_like_clarification_answer` function and 5 unit tests added at
  end of file.
- `crates/hydragent-tools/src/phase6.rs` — new `SanitizerListPatternsTool`
  (24 patterns supported, sourced from
  `config/security/injection_patterns.yaml` with built-in fallback).
- `crates/hydragent-core/src/main.rs` — `SecurityAction::VaultInit`
  variant added; `sanitizer_list_patterns` registered alongside the
  other Phase 6 tools.

### Test count

| Surface                                 | Tests | Status |
|-----------------------------------------|-------|--------|
| `hydragent-core` (incl. new heuristic)  | 34    | ✅     |
| All other crates                        | unchanged from 0.6.0 | ✅ |

## [0.6.0] — 2026-06-14

Phase 6 / Track 6.4: **Column-AES, mlock-pinned secrets, and zero-downtime
credential rotation**. The `hydragent-vault` crate grows four new modules
that turn the encrypted-blob vault into a hardened secret store: every
buffer holding a key, derived sub-key, or rotation report is now mlock-pinned
in RAM, zeroized on drop, and never serialised in the clear.

This release only covers Track 6.4 of Phase 6 (Weeks 23–26 of the roadmap).
The remaining Phase 6 tracks — Merkle audit chain (6.1), Ed25519 signing
(6.2), the `hydragent-security` pipeline crate — are still ahead. They will
land in subsequent 0.6.x releases.

**Scope decision (2026-06-14):** Track 6.5 (SQLCipher at-rest encryption for
`data/memory/`, `data/audit/`, `data/sessions/`) is **deferred to post-MVP**.
Column-AES inside the vault (Track 6.4) already protects the secrets; for
MVP, SQLite database files remain plaintext on disk.

### Highlights

- **AES-256-GCM column cipher** — `ColumnCipher` provides column-level
  encryption with the column name bound as additional authenticated data
  (AAD). Re-submitting a ciphertext with the wrong column name fails the
  GCM tag check, which prevents a "swap the column" attack.
- **HKDF-SHA256 sub-key derivation** — every column is encrypted with a
  fresh sub-key derived via HKDF from the master key. Rotating the column
  key invalidates *all* sub-keys, so old ciphertexts cannot be decrypted
  after rotation.
- **`mlock`-pinned `SecureBuffer<T>`** — a new heap-allocated,
  mlock-pinned, zeroize-on-drop buffer type. Failure to mlock is non-fatal
  (logged as a warning) but zeroize still runs.
- **Cross-platform `mlock`/`munlock`** — uses `libc::mlock` on Unix and
  `VirtualLock` on Windows. `is_mlock_available()` reports runtime support.
- **Passphrase + column key rotation** — `Rotator::rotate_passphrase`
  re-derives the master key, re-encrypts all entries, and atomically
  renames the temp file over the live vault. `Rotator::rotate_column_key`
  generates a new 32-byte column key, stores it as a special vault entry
  (`__column_key__`), and reports the new key as hex.
- **Vault wire format bumped** — `Vault::save` / `Vault::load` now
  cross the `TaintedString` boundary through a temporary `String`
  representation. The serialized file format itself is unchanged.

### Added (Track 6.4)

- `crates/hydragent-vault/src/mlock.rs` — cross-platform mlock wrapper
  (`MlockError`, `mlock`, `munlock`, `is_mlock_available`).
- `crates/hydragent-vault/src/secure_buffer.rs` — `SecureBuffer<T: Zeroize>`
  with mlock pinning and zeroize-on-drop semantics. Implements `Send`
  (when `T: Send`) but **not** `Sync` — the buffer is exclusively owned.
- `crates/hydragent-vault/src/column_cipher.rs` — `ColumnCipher` with
  per-column HKDF-derived sub-keys, AAD binding, and `fresh_key()`.
- `crates/hydragent-vault/src/rotator.rs` — `Rotator` with
  `rotate_passphrase` and `rotate_column_key`, plus `RotationReport`
  and the `__column_key__` constant.
- `RELEASE_NOTES_v0.6.0.md` — user-facing release notes.
- New workspace dependencies: `aes-gcm 0.10`, `hkdf 0.12`, `libc 0.2`
  (Unix), `windows-sys 0.59` (Windows).

### Test count

| Surface                                | Tests | Status |
|----------------------------------------|-------|--------|
| `hydragent-vault` (unit)               | 56    | ✅     |
| `hydragent-vault` (doctest)            | 1     | ✅     |
| `hydragent-vault` (integration: column_cipher) | 7     | ✅     |
| `hydragent-vault` (integration: rotation)      | 9     | ✅     |
| `hydragent-vault` (integration: secure_buffer) | 6     | ✅     |
| **Track 6.4 total**                    | **79** | ✅    |

All Phase 1–5 tests remain green; the full repo exceeds 380 tests.

### Migration from 0.5.x

No breaking changes for downstream consumers — Track 6.4 is purely
additive to `hydragent-vault`. If you were calling
`Vault::save`/`Vault::load` with a `HashMap<String, TaintedString>`,
nothing changes: those calls still work, and the new `Rotator` /
`ColumnCipher` / `SecureBuffer` types are exposed as additional public
API.

> **Platform note**: on Linux, `mlock` may require `CAP_IPC_LOCK` or root
> to succeed. On Windows, `VirtualLock` succeeds for any process, but
> pages are still pageable at the OS's discretion. In both cases, the
> zeroize-on-drop guarantee still holds regardless of mlock success.

[Unreleased]: https://github.com/your-org/hydragent/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/your-org/hydragent/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/your-org/hydragent/compare/v0.4.0...v0.5.0

Phase 5: **Subagent Swarm & Model Council** — hydragent stops being a single brain
and becomes a coordinated intelligence. A DAG planner decomposes complex tasks,
a pool of specialist sub-agents executes in parallel with scoped prompts and
tools, a Model Council routes each sub-task to the best-fit LLM, and a
self-healing re-planner recovers from failures autonomously.

### Highlights

- **DAG planner** — declarative task graphs with topo-sorted scheduling, JSON
  persistence, and a complexity classifier that turns user prompts into a
  ready-to-execute `DagSpec`.
- **Sub-agent spawner** — bounded-concurrency pool of specialist agents with
  scoped system prompts, tool allowlists, and a default tool/role mapping.
- **Model Council** — 23 LLM profiles loaded from `config/model_council.yaml`,
  routed per-role by `SubAgentRole`. `SubAgentStatus.model_used` now reports
  the actually-routed model id, not just the provider.
- **Mailbox** — file-based async messaging between sub-agents, with sequence
  numbers, per-recipient filtering, and a "wait for mail" primitive.
- **DAG execution engine** — runs a `DagSpec` to completion, respecting
  ready-queue ordering, parallelism caps, and cancellation. Emits a full
  `ExecutionReport` with per-node status, durations, tokens, and the final
  spec.
- **Supervisor** — synthesises multiple sub-agent outputs into a single
  answer, with a primary path (synthesis prompt to the council-routed model)
  and a deterministic fallback (concatenation).
- **Self-healing re-planner** — four strategies (`Retry`, `Reroute`,
  `Decompose`, `Escalate`) with a stochastic bias-driven selector and a
  deterministic `force_strategy` API for tests. Mutates the spec so the
  engine will re-pick the recovered node on the next pass.
- **Wiki** — on-disk Markdown knowledge base, atomic writes, topic
  sanitisation, case-insensitive search across all topics.
- **ASCII DAG printer** — fixed-width terminal output for live debugging
  (the `swarm_status` CLI and the `ascii` module).
- **Status CLI** — `swarm_status` binary consumes a `DagSpec` or
  `ExecutionReport` JSON and prints a human-readable picture.

### Added (Phase 5)

- `crates/hydragent-planner` — DAG types, scheduler, decomposer, serializer,
  execution engine, replanner, wiki, ASCII printer.
- `crates/hydragent-swarm` — sub-agent, spawner, coordinator, mailbox,
  supervisor.
- `crates/hydragent-model` — `ModelProfile`, `ModelCouncil` (5.2).
- `crates/hydragent-core` — Phase 5 binary that ties it all together.
- `bin/swarm_status` — Phase 5 / Track 5.4 diagnostic CLI.
- `config/model_council.yaml` — 23 LLM profiles with role hints.
- `tests/stress_test_phase5.py` — infrastructure smoke test for Phase 5.
- `RELEASE_NOTES_v0.5.0.md` — user-facing release notes.

### Test count

| Surface                          | Tests | Status |
|----------------------------------|-------|--------|
| `hydragent-planner` (lib)        | 36    | ✅     |
| `hydragent-planner` (integration)| 22    | ✅     |
| `hydragent-planner` (doctest)    | 3     | ✅     |
| `hydragent-swarm` (lib)          | 29    | ✅     |
| `hydragent-swarm` (integration)  | 46    | ✅     |
| `hydragent-swarm` (doctest)      | 2     | ✅     |
| **Phase 5 total**                | **138** | ✅   |

(Carried-over suites from Phases 1–4 are also green; totals for the whole
repo exceed 300 tests.)

### Deviations from `TODO_PHASE5.md`

- **Wiki and ASCII printer live in `hydragent-planner`, not `hydragent-core`.**
  The original plan put them in `hydragent-core`, but `hydragent-core` is
  a binary-only crate (it has no `lib.rs`) and other crates cannot depend
  on it. Both modules are also consumed by the `swarm_status` CLI binary
  and by integration tests, both of which need a library target. The
  decision keeps the planner crate cohesive (everything DAG-related in
  one place) and avoids creating a 14th workspace member just to host
  ~1000 lines of renderer / storage code.

### Migration from 0.4.x

No breaking changes for downstream consumers — Phase 5 is purely additive.
If you were using `hydragent-core`'s binary directly, you now have access to
`swarm_status` for post-run inspection and `hydragent-planner`'s modules
(including `wiki`, `replan`, `ascii`) as public API.

[Unreleased]: https://github.com/your-org/hydragent/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/your-org/hydragent/compare/v0.4.0...v0.5.0
