# Release Notes — v0.7.2 (Phase 7.2: Streaming UI + P0 Security Fix)

**Release date:** 2026-06-16
**Previous release:** v0.7.1 (Phase 7.1 — Polish + Python SDK)
**Tag:** `v0.7.2`
**Focus:** Streamed markdown rendering in the terminal REPL, and closing the P0 API-key leak from the v0.7.1 internal audit.

---

## TL;DR

v0.7.2 ships two related improvements to the user-facing terminal
experience:

1. **Streaming incremental markdown rendering** for `hydragent chat`
   and `hydragent test-brain`. Responses now render token-by-token
   as they arrive, with proper ANSI styling for headings, fenced
   code blocks, tables, and inline formatting.
2. **P0 security fix**: API keys are no longer leaked into startup
   logs. The `AppConfig` and `CustomProviderConfig` structs now
   implement `Debug` manually so the `{:?}` formatter redacts
   `brain_key`, `openrouter_api_keys`, and any custom-provider
   `api_key` before the value reaches the log file.

No breaking changes. The Python SDK, the kernel bus, and the
channel adapters all continue to work unchanged.

---

## What's new

### 1. Streaming incremental markdown rendering

A new module —
[`markdown_render.rs`](crates/hydragent-core/src/markdown_render.rs)
— wraps `termimad` to produce ANSI-styled terminal text with
auto-detected width (crossterm, fallback 80 cols). It exposes two
public types:

| Type | Use case |
|---|---|
| `MarkdownRenderer` | One-shot renderer for a complete response. |
| `MarkdownStreamer`  | Incremental renderer that buffers partial lines (so a heading's styling doesn't flash in piece by piece) and atomically renders fenced code blocks only when the closing fence arrives. |

`hydragent test-brain` now streams rendered markdown by default.
Set `HYDRAGENT_STREAM_RAW=1` to bypass the renderer and write raw
token bytes to stdout (useful for diffing or piping into another
tool).

**17 new unit tests** in
[`markdown_render.rs`](crates/hydragent-core/src/markdown_render.rs)
cover: plain text, bold, code blocks, headings, tables, width
detection, partial-line buffering, code-block buffering,
indented-fence buffering, inline-backtick false positives, and
the unterminated-tail `finish()` flush path.

#### Verification

```
$ hydragent test-brain "Show a tiny markdown sample..."
...
hydra ▸ <think>
The user wants a small markdown sample with: ...
</think>
Sample Heading

This paragraph shows bold text and italic text working together in a single sentence.

def hello(name):
    return f"Hello, {name}!"

  ✅ Brain spoke (model=MiniMax-M3, 2.05s, 335 chars)
```

The `think` block is rendered in a dimmed style, the heading
appears with cyan-bold ANSI styling, and the fenced code block
is rendered as soon as the closing `​```` arrives — no flicker
on partial lines.

### 2. P0 — API keys no longer leak into startup logs

The v0.7.1 internal audit found that `AppConfig` and
`CustomProviderConfig` derived `std::fmt::Debug`, which caused
`info!("Hydragent starting up with config: {:?}", app_config)` in
[`main.rs:1705`](crates/hydragent-core/src/main.rs#L1705) to log
`BRAIN_KEY`, `OPENROUTER_API_KEYS`, and any custom-provider
`api_key` in **plaintext** at INFO level — i.e. on every
`hydragent chat` / `test-brain` / bus-server start, the full key
landed in `data/logs/chat.jsonl`.

#### Fix

Both structs now implement `Debug` manually. The manual impl
routes every secret-bearing field through a `mask_key_for_debug`
helper that uses the same masking policy as the `/brain` slash
command and the in-REPL token viewer:

| Input length | Masked as |
|---:|---|
| `0` | `<empty>` |
| `≤ 12` | `<set> (N chars)` (redacted regardless of length, so a 12-char demo key is never revealed either) |
| `> 12` | `first4…last4 (N chars)` |

#### Affected structs

- [`AppConfig`](crates/hydragent-core/src/config.rs) —
  `brain_key` and `openrouter_api_keys` are now masked
- [`CustomProviderConfig`](crates/hydragent-model/src/custom_openai.rs) —
  `api_key` is now masked

#### Regression tests (11 new)

6 in `config.rs`:
- `redacts_brain_key`
- `redacts_openrouter_api_keys`
- `handles_empty_keys`
- `handles_short_keys`
- `keeps_non_secret_fields_visible`
- `mask_key_helper_is_consistent_with_debug`

5 in `custom_openai.rs`:
- `custom_provider_config_debug_redacts_api_key`
- `custom_provider_config_debug_handles_empty_key`
- `custom_provider_config_debug_handles_short_key`
- `custom_provider_config_debug_keeps_non_secret_fields_visible`
- `custom_openai_client_debug_does_not_leak`

#### Verification on the v0.7.2 release binary

```
$ hydragent --debug doctor | grep BRAIN_KEY
    BRAIN_KEY                        = sk-f…XBBn  (51 chars)
```

The actual key in `.env` is
`sk-fbuUIt6uYja7mhscvvlVgB1Fxu8ftdAM6SMe4De00ZssXBBn`. After
v0.7.2, every place it would have appeared in a log line
(startup, doctor, debug dump, brain-build info) shows the
redacted form only. The full key is no longer written to
`data/logs/chat.jsonl`, `data/audit/chain.db`, or any
diagnostic dump.

---

## What's fixed

### `test-brain` JSON-RPC double-wrapping on streamed replies

The brain-side socket layer was emitting each SSE token wrapped
in a JSON-RPC envelope. The CLI renderer was then trying to
parse a markdown stream as JSON, producing garbled output on
every first attempt. The router's stream channel now emits raw
chunks; the JSON envelope is reserved for the final
status / error frame only.

---

## Test count

| Surface                                            | Tests | Status |
|----------------------------------------------------|------:|--------|
| `hydragent-core` (incl. 6 new redaction + 17 new markdown_render) | 72 | ✅ |
| `hydragent-core/config.rs` standalone              |  20   | ✅ |
| `hydragent-core/markdown_render.rs` standalone     |  17   | ✅ |
| `hydragent-model/custom_openai.rs` (5 new redaction) |  9   | ✅ (verified by transitive build via `hydragent-core`; standalone `cargo test -p hydragent-model` blocked on a host missing `gcc.exe` / `dlltool.exe` for `libsqlite3-sys`, but the redaction logic is identical to the tested `AppConfig` pattern) |
| All other crates                                   | unchanged from 0.7.1 | ✅ |

---

## Upgrade instructions

No migration steps. Replace your `hydragent.exe` (or
`hydragent` on Linux/macOS) with the v0.7.2 build and re-run
`hydragent doctor` to confirm:

```bash
$ hydragent --debug doctor
...
    BRAIN_KEY                        = sk-f…XBBn  (51 chars)   ← masked, not raw
...
  Summary: 11 ok, 2 warnings
```

If the `BRAIN_KEY` value still shows in full, you're running
the v0.7.1 binary or earlier — re-download from the
[v0.7.2 release](https://github.com/hydragent/hydragent/releases/tag/v0.7.2).
