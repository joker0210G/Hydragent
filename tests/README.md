# Tests

End-to-end and integration tests for the Hydragent runtime. Most tests are
Python and exercise the **live** bus (`hydragent.exe` running on
`127.0.0.1:5000`) plus the live LLM brain (no mocks).

## Layout

```
tests/
├── cli_user_pov.py            # User-perspective chat smoke test (Phase 7)
├── smoke_websocket.py         # WS adapter end-to-end smoke
├── start_bus.py               # Background-launch the hydragent bus
├── start_websocket_adapter.py # Background-launch the WS adapter
├── test_searchxng_e2e.py      # SearXNG-backed web_search E2E
├── test_ws_push_e2e.py        # WS push routing E2E
├── probe_searxng_stdlib.py    # Find a working public SearXNG instance
├── bench/                     # Bench data (JSONL golden sets)
│   ├── golden_set_v1.jsonl
│   └── skill_bench_v1.jsonl
└── legacy/                    # Historical phase tests (completed)
    ├── stress_test_phase1.py
    ├── stress_test_phase2.py
    ├── stress_test_phase3.py
    ├── stress_test_phase3_user.py
    ├── stress_test_phase5.py
    ├── demo_phase5.bat
    └── phase6_user_pov.md
```

## Active tests (at root)

| File | What it does | How to run |
|---|---|---|
| `cli_user_pov.py` | Drives 4 prompts through the bus the way the real CLI adapter does — same connection lifecycle, same callbacks. Used as the chat smoke test for Phase 7. | Start `hydragent.exe` in a terminal, then `python tests/cli_user_pov.py` |
| `smoke_websocket.py` | Connects a WS client, sends a trivial prompt (`2+2`), asserts the response. | Start bus + WS adapter, then `python tests/smoke_websocket.py` |
| `start_bus.py` | Launches `hydragent.exe` in the background, waits for port 5000, writes PID file. Idempotent. | `python tests/start_bus.py` |
| `start_websocket_adapter.py` | Launches the WS adapter in the background, waits for port 8765, writes PID file. Idempotent. | `python tests/start_websocket_adapter.py` |
| `test_searchxng_e2e.py` | 4 prompts that exercise ReactLoop, web_search tool, swarm fan-out, and AskUser clarification. | Start bus + SearXNG shim, then `python tests/test_searchxng_e2e.py` |
| `test_ws_push_e2e.py` | Asks the LLM to use `send_message` to push to a target page, asserts the target receives it and others don't. | Start bus + WS adapter, then `python tests/test_ws_push_e2e.py` |
| `probe_searxng_stdlib.py` | Tries 30 public SearXNG instances with a realistic User-Agent, prints a recommended `SEARXNG_BASE_URL=...` line for the first one that works. No external deps. | `python tests/probe_searxng_stdlib.py` |

## Bench data

The `bench/` JSONL files are the bench harness's golden sets, consumed
directly by the Rust harness in `crates/hydragent-bench/`. They are
read-only data, not executable tests. To run the bench:

```
cargo test -p hydragent-bench
```

See `doc/STATE.md` and `RELEASE_NOTES_v0.7.0.md` for what's in each
JSONL (SKILL-BENCH 80 tasks / Golden Set 30 pairs as of v0.7.0).

## Legacy (historical)

`tests/legacy/` holds the stress tests and user-perspective plans from
**completed phases** (1, 2, 3, 5, 6). They are kept for reference and
historical context but are not part of the active CI loop. If you need
to re-run one, the file's docstring has the usage instructions.

## Conventions

- All Python tests are runnable as `python tests/<file>.py` from the
  workspace root. They add `adapters/` to `sys.path` to import
  `bus_client` / `websocket_adapter`.
- Tests are **integration tests**, not unit tests. They require a
  running `hydragent.exe` (and sometimes the WS adapter) to be present.
- Exit code: `0` on success, `1` on any assertion failure. Most tests
  also print a per-step PASS/FAIL line.
- Use `start_bus.py` / `start_websocket_adapter.py` to bring the
  runtime up before running the E2E tests. Both are idempotent and
  re-use an already-running process.
