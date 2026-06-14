# Hydragent — Current TODO

_Last updated: 2026-06-14 — post-v0.7.0, pre-Phase 8_

> **Current state**: v0.7.0 (Hermes) shipped. 567 tests passing, 1 pre-existing
> failure (`custom_openai_integration`, unrelated). All Phase 1–7 work is
> complete and archived. See **[`CHANGELOG.md`](CHANGELOG.md)** for the
> version history and **[`PHASE_7_COMPLETION_SUMMARY.md`](PHASE_7_COMPLETION_SUMMARY.md)**
> for the most recent phase retrospective.

---

## 🎯 Next up (Phase 8 candidates)

| # | Item | Notes |
|---|---|---|
| 1 | **Tag `v0.7.0`** | Pending user confirmation. Includes full release notes + bench baseline. |
| 2 | **Wire a ReAct agent to SKILL-BENCH** | Current `reports/bench-v0.7.0.json` is all-zero (retriever stub). Need a real R@1 baseline. |
| 3 | **Skill induction at scale** | 1,000+ trajectories → 100+ candidate skills. Curator + executor paths already exist. |
| 4 | **Swarm tool registry** | Sub-agents don't inherit the main agent's `skill_*` tools (finding from v0.7.0 chat testing). Phase 8 work for the planner/swarm crates. |
| 5 | **Fix pre-existing test failure** | `hydragent-model --test custom_openai_integration::custom_provider_streams_openai_chunks` — protocol-format mismatch in the test fixture. Not Phase 7. |
| 6 | **Adversarial skill-induction eval** | Red-team with poisoned trajectories. |

> No in-progress work as of v0.7.0. The list above is the *next* set, not what's currently half-done.

---

## 📂 Archive

Completed-phase reports and older release notes have moved to `doc/archive/`:

```
doc/archive/
├── phases/                              # Completed-phase retrospectives
│   ├── PHASE_2_FINAL_REPORT.md          # 22/22 tests pass; HNSW + FTS5 fixes
│   ├── PHASE_4_FINAL_REPORT.md          # WebSocket channel + 1 deferred issue
│   ├── PHASE_5_FINAL_REPORT.md          # Swarm + Model Council + replanner
│   └── PHASE_7_TODO.md                  # Per-day tracker (per-week summary in
│                                        #   PHASE_7_COMPLETION_SUMMARY.md at root)
└── releases/                            # Older release notes
    ├── RELEASE_NOTES_v0.5.0.md          # The Swarm
    └── RELEASE_NOTES_v0.6.0.md          # Locked Memory
```

The current release notes ([`RELEASE_NOTES_v0.7.0.md`](RELEASE_NOTES_v0.7.0.md)) and the
most-recent phase retrospective ([`PHASE_7_COMPLETION_SUMMARY.md`](PHASE_7_COMPLETION_SUMMARY.md))
stay at the root for visibility.

---

## 📂 Files of interest (v0.7.0)

- `tests/cli_user_pov.py` — user-perspective chat smoke test
- `tests/README.md` — tests folder layout
- `target\debug\hydragent.exe` — bus binary
- `data/sessions.db` — SQLite (WAL mode)
- `data/skill_library.sqlite` — skill library (3 builtins seeded)
- `current-idea-library-graph.md` — speculative design doc (graph + Library metaphor)

---

## 🔧 Bus invocation reference

```cmd
cd /d "F:\Workspace(temp)\repo\ai agent"
set RUST_BACKTRACE=full
start /B target\debug\hydragent.exe > %TEMP%\bus_dbg.out 2> %TEMP%\bus_dbg.err
```

Bus listens on `127.0.0.1:5000` (JSON-RPC 2.0 over TCP).

For the WebSocket adapter (optional, for chat testing):

```cmd
adapters\.venv\Scripts\python.exe tests\start_websocket_adapter.py
```

WebSocket on `127.0.0.1:8765` (path `/ws`).
