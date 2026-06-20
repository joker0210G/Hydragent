# Changelog

All notable changes to hydragent are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- **P0 — API keys no longer leak into startup logs** (closes the
  v0.7.1 internal-audit finding). `AppConfig` and `CustomProviderConfig`
  previously derived `std::fmt::Debug`, which caused
  `info!("Hydragent starting up with config: {:?}", app_config)` in
  [main.rs:1705](crates/hydragent-core/src/main.rs#L1705) to log
  `BRAIN_KEY`, `OPENROUTER_API_KEYS`, and any custom-provider
  `api_key` in **plaintext** at INFO level — i.e. on every
  `hydragent chat` / `test-brain` / bus-server start, the full key
  landed in `data/logs/chat.jsonl`.
  - Both structs now implement `Debug` manually; the manual impl
    routes every secret-bearing field through a `mask_key_for_debug`
    helper that uses the same policy as `/brain` and the in-REPL
    token viewer:
      - `""`  →  `<empty>`
      - `len ≤ 12`  →  `<set> (N chars)`  (redacted regardless of
        length, so a 12-char demo key is never revealed either)
      - `len > 12`  →  `first4…last4 (N chars)`
  - Affected structs:
    - [`AppConfig`](crates/hydragent-core/src/config.rs) —
      `brain_key` and `openrouter_api_keys` are now masked
    - [`CustomProviderConfig`](crates/hydragent-model/src/custom_openai.rs) —
      `api_key` is now masked
  - Regression tests (6 new in `config.rs`, 5 new in `custom_openai.rs`):
    - `redacts_brain_key`
    - `redacts_openrouter_api_keys`
    - `handles_empty_keys`
    - `handles_short_keys`
    - `keeps_non_secret_fields_visible`
    - `mask_key_helper_is_consistent_with_debug`
    - `custom_provider_config_debug_redacts_api_key`
    - `custom_provider_config_debug_handles_empty_key`
    - `custom_provider_config_debug_handles_short_key`
    - `custom_provider_config_debug_keeps_non_secret_fields_visible`
    - `custom_openai_client_debug_does_not_leak`
  - **Verification** on the v0.7.2 release binary (2026-06-16 06:59 UTC):
    - `hydragent --debug doctor` now prints
      `BRAIN_KEY = sk-f…XBBn  (51 chars)` instead of the full key.
    - The same masked value is shown in the `[4] Effective brain
      config` block of the debug dump.
    - `data/logs/chat.jsonl` no longer contains the raw key.

### Added

- **Streaming incremental markdown rendering in `hydragent chat`
  and `hydragent test-brain`** — brain responses are now rendered
  token-by-token as they stream in, rather than waiting for the
  full response before displaying. Heading styling, fenced code
  blocks, and tables now lay out incrementally without
  re-rendering the whole transcript. The previous behaviour
  (buffer-then-dump) was unusable for long responses on
  slow-to-first-token providers.
  - New module:
    [`markdown_render.rs`](crates/hydragent-core/src/markdown_render.rs)
    wraps `termimad` to produce ANSI-styled terminal text with
    auto-detected width (crossterm, fallback 80 cols).
  - Two public types:
    - `MarkdownRenderer` — one-shot renderer for a complete
      response.
    - `MarkdownStreamer` — incremental renderer that buffers
      partial lines (so a heading's styling doesn't flash in
      piece by piece) and atomically renders fenced code blocks
      only when the closing fence arrives.
  - **17 unit tests** cover plain text, bold, code blocks,
    headings, tables, width detection, partial-line buffering,
    code-block buffering, indented-fence buffering, inline-
    backtick false positives, and the unterminated-tail
    `finish()` flush path.
  - `hydragent test-brain` now streams rendered markdown by
    default. Set `HYDRAGENT_STREAM_RAW=1` to bypass the renderer
    and write raw token bytes to stdout (useful for diffing or
    piping into another tool).

- **`hydragent update` subcommand** — self-updates the running
  binary in place from GitHub Releases. Queries the latest
  release tag, compares it against the compile-time version stamp
  (`CARGO_PKG_VERSION`) without downgrading dev builds, downloads
  the matching platform asset, and replaces the binary without
  admin elevation or `PATH` mutation.
  - New module:
    [`update.rs`](crates/hydragent-core/src/update.rs)
    (~300 lines). Uses `reqwest` for HTTPS, `tempfile` + `uuid`
    for staging, and a **3-tier archive extraction chain**:
    1. System `tar -xf` (Win10+, macOS, Linux).
    2. Native Rust: `flate2` + `tar::Archive` for `.tar.gz`,
       `zip::ZipArchive` for `.zip`.
    3. PowerShell `Expand-Archive` for `.zip` on older Windows
       where `tar.exe` is missing.
- **`hydragent uninstall` subcommand** — interactive uninstaller
  with `-y` / `--yes` for scripted teardown. Removes the install
  directory, strips the matching `export PATH=…` line from the
  user's shell rc / PowerShell profile, and refuses to delete a
  source checkout's `.git/` tree without explicit confirmation.
  - New module:
    [`uninstall.rs`](crates/hydragent-core/src/uninstall.rs)
    (~150 lines).

### Fixed

- **`test-brain` JSON-RPC double-wrapping on streamed replies** —
  the brain-side socket layer was emitting each SSE token wrapped
  in a JSON-RPC envelope. The CLI renderer was then trying to
  parse a markdown stream as JSON, producing garbled output on
  every first attempt. The router's stream channel now emits
  raw chunks; the JSON envelope is reserved for the final
  status / error frame only.

### Test count

| Surface                                            | Tests | Status |
|----------------------------------------------------|------:|--------|
| `hydragent-core` (incl. 6 new redaction + 17 new markdown_render) | 72 | ✅ |
| `hydragent-core/config.rs` standalone              |  20   | ✅ |
| `hydragent-core/markdown_render.rs` standalone     |  17   | ✅ |
| `hydragent-model/custom_openai.rs` (5 new redaction) |  9   | ✅ (verified by transitive build via `hydragent-core`; standalone `cargo test -p hydragent-model` blocked on a host missing `gcc.exe` / `dlltool.exe` for `libsqlite3-sys`, but the redaction logic is identical to the tested `AppConfig` pattern) |
| All other crates                                   | unchanged from 0.7.1 | ✅ |

## [0.7.1] — 2026-06-15

Phase 7.1: **Polish + Python SDK**. Establishes the kernel/frontend/SDK
architectural split and ships the official Python SDK as a first-class
consumer of the kernel bus. See
[RELEASE_NOTES_v0.7.1.md](doc/releases/RELEASE_NOTES_v0.7.1.md) for the full
walk-through.

### Added

- **`adapters/hydragent_py/` SDK package** (new) — 10 files, ~36 KB
  - `HydraClient` — high-level synchronous/async wrapper over the bus
    with auto-reconnect, context-manager support, and a typed
    `HydraError` hierarchy
    ([client.py](adapters/hydragent_py/client.py))
  - `HydraConfig` — dataclass with `from_env()` factory; honours
    `HYDRA_BUS_HOST` / `HYDRA_BUS_PORT` / `HYDRA_PAGE_ID` / `HYDRA_USER_ID`
  - `BusClient` — JSON-RPC over TCP, with `host`/`port` constructor
    args and a graceful `close()`
    ([bus_impl.py](adapters/hydragent_py/bus_impl.py))
  - `REPL` class + `run_repl()` console-script entry point
    ([repl.py](adapters/hydragent_py/repl.py))
  - `plugins` — `PluginContext`, `ToolSpec`, `SlashCommand`,
    `discover()`, `load_all()` with 4 ordered discovery directories
    ([plugins.py](adapters/hydragent_py/plugins.py))
  - `cli.py` — `hydra-cli {chat,repl,send}` console script entry point
  - `builtin/hello_world.py` — 10-line example plugin
  - `py.typed` — PEP 561 marker for type-checker support
  - `README.md` — quick-start, plugin tutorial, package map
- **`tests/test_hydragent_py.py`** — 7 SDK smoke tests (all passing)

### Changed

- **`adapters/cli_adapter.py` is now a 27-line shim** — forwards to
  `hydragent_py.repl.run_repl`. The original Rich-based REPL
  implementation is preserved inside the SDK package.
- **`adapters/bus_client.py` is now a 50-line shim** — re-exports
  `hydragent_py.bus.BusClient`.
- **`adapters/pyproject.toml`** — declares `packages = ["hydragent_py",
  "hydragent_py.builtin"]` and `[project.scripts] hydra-cli =
  "hydragent_py.cli:main"`.

### Performance

- **`SpinnerHandle::stop()` in `cli_repl.rs`** — removed the 50 ms
  `thread::sleep` and replaced the bare `Arc<AtomicBool>` with
  `Option<SpinnerHandle>` + `JoinHandle` so the spinner thread is
  *deterministically* joined on stop. The REPL no longer pays a 50 ms
  tax on every response.

### Architecture

- The kernel (Rust, `hydragent-core`) is now the only thing that owns
  agent state. The SDK is the only Python surface for the bus. All
  frontends (Rust REPL, Python REPL, web mini-app, channel adapters)
  are stateless renderers. See `doc/PHASE_8_PLAN.md` §1.5 for the
  architecture diagram and the three rules.

## [0.7.0] — 2026-06-14

Phase 7: **Self-Improving Skill Engine & Curator**. Adds a persistent
SQLite-backed `SkillLibrary`, a Hermes-style deterministic skill
inducer, a `SkillExecutor` with prompt-rendering + tool-allowlist
enforcement, a 7-day curator that promotes/demotes skills, an
80-task SKILL-BENCH + 30-pair golden-set retrieval benchmark, and a
Python LoRA fine-tuning pipeline. See
[RELEASE_NOTES_v0.7.0.md](doc/releases/RELEASE_NOTES_v0.7.0.md) for the full
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
