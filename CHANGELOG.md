# Changelog

All notable changes to hydragent are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] — 2026-06-13

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
