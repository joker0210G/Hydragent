# Scratch pad — search & fetch system

> Living review of the 3-tier web tool surface. Updated 2026-06-20 after rebuild.

---

## Current state (2026-06-20)

The system is a **5-tier fallback chain** (was 3-tier at first review):

1. **SearXNG** (configured base) → JSON API
2. **SearXNG** (rotated public instances, deduped) → JSON API
3. **DuckDuckGo Lite** (`lite.duckduckgo.com/lite/`) → HTML scrape
4. **DuckDuckGo HTML** (`html.duckduckgo.com/html/`) → HTML scrape, different class names
5. **No-results emergency** — `rel="nofollow"` regex extraction as last resort

`url_fetch` no longer hits the origin server directly; it routes every URL through **`https://r.jina.ai/`** which returns clean, LLM-friendly article text (no ads, nav, scripts). Local SSRF guard still blocks private/loopback/link-local/multicast IPs before the request leaves.

The ReAct system prompt now has explicit anti-drift guardrails (see §3 below).

---

## 1. Status of the original 9 fixes

| # | Issue | Fix | Where | Status |
|---|---|---|---|---|
| 1 | UTF-8 panic on multi-byte chars in `url_fetch` | `truncate_chars` uses `chars().count()` / `chars().take()`, never byte slicing | [url_fetch.rs:75-83](crates/hydragent-tools/src/url_fetch.rs#L75-L83) | ✅ |
| 2 | `searx.be` doubled in fallback chain | `.filter(\|&&u\| u != self.base_url)` | [web_search.rs:489-497](crates/hydragent-tools/src/web_search.rs#L489-L497) | ✅ |
| 3 | SSRF — internal IP access | `validate_url()` blocks loopback / private / link-local / multicast / unspecified IPs + localhost hostname | [url_fetch.rs:46-93](crates/hydragent-tools/src/url_fetch.rs#L46-L93) | ✅ |
| 4 | Unbounded body read — 500 MB DoS | `MAX_BODY_BYTES = 25 MB` with `content_length` header pre-check + post-read re-check | [url_fetch.rs:236-269](crates/hydragent-tools/src/url_fetch.rs#L236-L269) | ✅ |
| 5 | Raw HTML returned to LLM | `strip_html_tags()` runs before truncation. **Plus** every fetch now goes through Jina → pre-cleaned article text. | [url_fetch.rs:280-285](crates/hydragent-tools/src/url_fetch.rs#L280-L285) | ✅ (superseded by Jina) |
| 6 | Brittle DDG regex | `.or_else()` with two regex variants — handles both `class=` first and `href=` first attribute orders | [web_search.rs:233-245](crates/hydragent-tools/src/web_search.rs#L233-L245) | ✅ (see §2 below — quote-agnostic version now in place) |
| 7 | `&nbsp;` + numeric entities not decoded | 12 entities decoded including `&nbsp;`, `&apos;`, `&#x27;`, `&#x2F;`, `&#x3E;`, `&#x3C;`, `&#x22;` | [web_search.rs:555-568](crates/hydragent-tools/src/web_search.rs#L555-L568) | ✅ in web_search — ❌ still missing in url_fetch |
| 8 | Dead `_start` parameter | Parameter removed entirely from `search_ddg_lite` signature | [web_search.rs:215-218](crates/hydragent-tools/src/web_search.rs#L215-L218) | ✅ |
| 9 | No tests for SSRF / truncation / etc. | 11 tests in `url_fetch` (was 2) + 9 tests in `web_search` for truncate / build_url / parse | [url_fetch.rs:296-396](crates/hydragent-tools/src/url_fetch.rs#L296-L396) | ✅ |

---

## 2. The 4 fixes added since the last review

These came out of a live test failure — `web_search` kept returning empty and the model hallucinated about "Fable 5 game".

### 2a. `web_search` was returning 0 results for some queries

**Three compounding root causes:**

| Sub-fix | What changed | Where |
|---|---|---|
| **Quote-agnostic DDG regex** | Old regex only matched double-quoted `class="…"`. DDG Lite sometimes sends single quotes `class='…'`. New regex uses `['\"]` character class for both. | [web_search.rs:236-237](crates/hydragent-tools/src/web_search.rs#L236-L237) (DDG Lite), [web_search.rs:301-302](crates/hydragent-tools/src/web_search.rs#L301-L302) (DDG HTML) |
| **No-results emergency fallback** | If DDG returns 0 titles but the page is non-empty, a third regex matches `rel="nofollow"` + absolute `https?://` as a last-resort recovery. Stable across DDG redesigns. | [web_search.rs:269-292](crates/hydragent-tools/src/web_search.rs#L269-L292) |
| **DDG HTML as secondary backend** | New `search_ddg_html()` scrapes `html.duckduckgo.com/html/` which uses `class="result__a"` (not `result-link`). Acts as a safety net when Lite changes its markup. | [web_search.rs:296-355](crates/hydragent-tools/src/web_search.rs#L296-L355) |

The execute path now chains: **SearXNG (rotated) → DDG Lite → DDG HTML**, with the Lite-empty fallback to HTML even if Lite didn't error.

### 2b. `url_fetch` now routes through Jina AI Reader

```rust
// url_fetch.rs ~ line 198
let jina_url = format!("https://r.jina.ai/{}", url);
```

- `r.jina.ai` is a public web-to-text extractor (no API key needed for moderate use).
- Returns **clean article text** — no nav, ads, scripts, footer.
- Roughly 10× smaller payloads → 10× fewer LLM tokens per page.
- Output includes `"via_jina": true` and `"jina_url"` for transparency.
- Local SSRF guard still fires first, so this *can't* be used as a bypass.

**Caveat:** Jina is a third-party dependency now. If `r.jina.ai` is down or rate-limits, every `url_fetch` call fails. Consider adding an env var `URL_FETCH_BACKEND=jina|direct` (default `jina`) so users can fall back without recompiling.

### 2c. CLI status parser was mis-reading every tool result

**Bug:** Old parser looked for `"success"` *inside* the `[Tool Result]` brackets, but the brackets were empty — the status word lives outside them.

**Fix:** [cli_repl.rs:1587-1590](crates/hydragent-core/src/cli_repl.rs#L1587-L1590) — after `strip_brackets("Tool Result")` succeeds, scan the **full unbackticked string** for lowercase `"status: success"`:

```rust
if strip_brackets(unbackticked, "Tool Result").is_some() {
    let ok = unbackticked
        .to_ascii_lowercase()
        .contains("status: success");
    return StatusKind::ToolResult { ok };
}
```

### 2d. ReAct prompt now has anti-drift / anti-hallucination guardrails

[react_loop.rs:69-77](crates/hydragent-core/src/react_loop.rs#L69-L77):

```text
ReAct Loop Rules (follow strictly):
- Trust live tool results over your training knowledge. If search results
  contradict what you know, believe the search.
- Stay STRICTLY on the user's topic. Do NOT rewrite their query into
  unrelated domains just because the first search is empty.
- If a search returns 0 results, say you could not find current information.
  Do NOT invent alternative queries about related topics.
- When search results contain promising URLs, use url_fetch to read the
  full page content before drawing conclusions.
- Do NOT answer from memory if you just ran a search — use what the search
  returned.
- Limit yourself to ONE search per topic unless the user explicitly asks
  for comparisons.
```

The description of `web_search` was also updated to instruct the LLM to follow up with `url_fetch` when results contain promising URLs — closes the loop between search and fetch.

---

## 3. Test verification (post-rebuild)

```
cargo build --release -p hydragent-core    →  Finished release in 2.69s
cargo test -p hydragent-tools             →  27 passed; 0 failed
```

| Crate | Tests | Change |
|---|---|---|
| `hydragent-core` | 98 | no change |
| `hydragent-tools` | 27 | +9 from this work |

Only warning is the pre-existing dead-code for `ANSI_BOLD` in `cli_repl.rs:64` — unrelated, was there before this work.

---

## 4. Remaining open issues

These weren't part of the recent round but are still worth knowing about:

| # | Issue | Severity | Effort |
|---|---|---|---|
| A | `url_fetch` strips tags but doesn't run `html_decode` — `&nbsp;` etc. still leak through to the LLM | 🟠 same-class bug | 5 min |
| B | DNS rebinding bypasses `validate_url` — URL string is checked, but reqwest re-resolves DNS at fetch time. An attacker-controlled DNS for `evil.com` can return `127.0.0.1`. | 🟠 real SSRF gap | 30 min |
| C | 25 MB body still loads into RAM before size check fires when `Content-Length` is missing. Use streaming `chunk()` with a counter. | 🟡 partial DoS | 15 min |
| D | `strip_html_tags` / `html_decode` / `truncate_chars` are duplicated in both `url_fetch.rs` and `web_search.rs`. Move to shared `crates/hydragent-tools/src/text.rs`. | 🟡 code smell | 15 min |
| E | `host == "localhost"` is case-sensitive — `LocalHost` bypasses the guard. Use `eq_ignore_ascii_case`. | 🟡 minor | 1 min |
| F | `content_length` field in `url_fetch` output reports truncated length, not original. Rename or split. | 🟡 minor | 1 min |

**Recommended next PR:** A + B + D together. ~50 min total, closes the last real correctness/security gaps.

---

## 5. What to expect after the rebuild

- `web_search` → falls back through **SearXNG → DDG Lite → DDG HTML** with emergency `rel="nofollow"` extraction as last resort.
- If results contain URLs, the model is instructed to follow up with `url_fetch`.
- The model will not rewrite your query into unrelated topics (no more "Fable 5 game").
- `url_fetch` returns clean, Jina-extracted article text instead of raw HTML.
- `★ failed` no longer flashes on successful tool calls — status parser matches the actual content.