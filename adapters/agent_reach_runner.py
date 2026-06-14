#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
adapters/agent_reach_runner.py
──────────────────────────────
Thin Python wrapper around Agent-Reach (https://github.com/Panniantong/Agent-Reach).
This script is the single subprocess entry-point that the hydragent-tools
`agent_reach` tool spawns. It does NOT auto-install anything — it only
*uses* the channels that `agent-reach install --safe` has already set up.

Channels supported (all "install-and-forget", no per-call credentials):
    jina_fetch   - Read any URL as Markdown via Jina Reader (r.jina.ai).
                   This is the universal web-reader (the "任意网页 ✅" channel).
    youtube      - Fetch metadata + subtitles for a YouTube URL via yt-dlp.
    bilibili     - Search Bilibili videos via the public search API.
    rss          - Fetch and parse an RSS/Atom feed.
    github       - Lightweight GitHub readme/code fetch via raw.githubusercontent
                   (no gh CLI required — that needs a system install we skip).
    doctor       - Return the Agent-Reach doctor report as JSON.

JSON I/O contract (must stay stable — Rust tool depends on it):
    stdin  (text)  : JSON object with at least {"command": "...", ...}
    stdout (text)  : JSON object on success
                     {"ok": true, "command": "...", "result": <varies>}
                     or on error
                     {"ok": false, "command": "...", "error": "<msg>",
                      "hint": "<what to do>"}

Exit code:
    0  success (including "no results" — the caller can decide)
    2  invalid params (caller should not retry)
    3  network / upstream error (caller may retry)
    4  missing dependency (caller must tell the user to run install)

Usage example (from Rust):
    stdin  = json.dumps({"command": "jina_fetch",
                         "url": "https://example.com"}).encode()
    stdout = subprocess.communicate(stdin)
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
import urllib.error
from typing import Any, Dict, List, Optional, Tuple

# Channel version — bump if contract changes.
__version__ = "1.0.0"

# Per-channel timeouts (seconds). Keep short — agent calls should not block.
TIMEOUT_JINA = 20
TIMEOUT_YT = 30
TIMEOUT_BILI = 15
TIMEOUT_RSS = 15
TIMEOUT_GH = 15

USER_AGENT = ("AgentReachRunner/1.0 (+https://github.com/Panniantong/Agent-Reach) "
              "hydragent-tools/agent_reach")


# ─────────────────────────────────────────────────────────────────────────────
# Output helpers
# ─────────────────────────────────────────────────────────────────────────────

def emit(payload: Dict[str, Any]) -> None:
    """Write one JSON object to stdout and exit 0."""
    sys.stdout.write(json.dumps(payload, ensure_ascii=False))
    sys.stdout.write("\n")
    sys.stdout.flush()


def emit_error(command: str, error: str, hint: Optional[str] = None,
               exit_code: int = 3, **extra: Any) -> None:
    payload: Dict[str, Any] = {
        "ok": False,
        "command": command,
        "error": error,
    }
    if hint:
        payload["hint"] = hint
    payload.update(extra)
    sys.stdout.write(json.dumps(payload, ensure_ascii=False))
    sys.stdout.write("\n")
    sys.stdout.flush()
    sys.exit(exit_code)


# ─────────────────────────────────────────────────────────────────────────────
# HTTP helpers — we use stdlib only so we don't need extra pip packages here.
# ─────────────────────────────────────────────────────────────────────────────

def _http_get(url: str, timeout: int,
              headers: Optional[Dict[str, str]] = None) -> Tuple[int, Dict[str, str], str]:
    """Plain stdlib GET. Returns (status, headers, body)."""
    req = urllib.request.Request(url, headers={
        "User-Agent": USER_AGENT,
        "Accept": "*/*",
        **(headers or {}),
    })
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return (resp.status, dict(resp.getheaders()), resp.read().decode("utf-8", errors="replace"))
    except urllib.error.HTTPError as e:
        body = ""
        try:
            body = e.read().decode("utf-8", errors="replace")
        except Exception:
            pass
        return (e.code, dict(e.headers or {}), body)
    except urllib.error.URLError as e:
        return (0, {}, f"URLError: {e.reason}")
    except Exception as e:
        return (0, {}, f"{type(e).__name__}: {e}")


# ─────────────────────────────────────────────────────────────────────────────
# Channel: jina_fetch — read any URL as Markdown via Jina Reader
# ─────────────────────────────────────────────────────────────────────────────
# r.jina.ai is a free, public, no-account service that:
#   - returns clean Markdown (no ads, no nav, no scripts)
#   - respects robots.txt in its own way
#   - handles JavaScript-rendered pages (it uses a headless browser internally)
# Reference: https://jina.ai/reader/
#
# Usage:
#   r.jina.ai/<URL>          → returns Markdown body, with optional X-Target-Selector
#   r.jina.ai/<URL>?json=true → returns {"data": {"content": "...", "title": "..."}}
#
# We always ask for JSON to get the structured {title, content} form.

def cmd_jina_fetch(params: Dict[str, Any]) -> Dict[str, Any]:
    url = (params.get("url") or "").strip()
    if not url:
        emit_error("jina_fetch", "Missing 'url' parameter",
                   "Pass {\"command\": \"jina_fetch\", \"url\": \"https://...\"}",
                   exit_code=2)
    if not re.match(r"^https?://", url):
        emit_error("jina_fetch", f"URL must start with http:// or https://, got: {url!r}",
                   exit_code=2)
    # Strip accidental trailing ?json=true or similar the caller may have added.
    if "?" in url:
        url = url.split("?", 1)[0]

    max_chars = int(params.get("max_chars", 12000))
    if max_chars < 500:
        max_chars = 500
    if max_chars > 100000:
        max_chars = 100000

    target_selector = params.get("selector")  # optional CSS selector

    # Build the Jina URL. Use json=true to get structured response.
    # Format: r.jina.ai/<URL>?<query>  — query params go on the OUTER URL,
    # not appended to the inner target. We must also send `Accept: application/json`
    # — without it, Jina returns plain Markdown and ignores ?json=true.
    qs: Dict[str, str] = {"json": "true"}
    if target_selector:
        qs["target-selector"] = target_selector
    jina_url = f"https://r.jina.ai/{url}?" + urllib.parse.urlencode(qs)

    status, headers, body = _http_get(jina_url, TIMEOUT_JINA,
                                      headers={"Accept": "application/json"})
    if status == 0:
        emit_error("jina_fetch", f"Jina Reader network error: {body}",
                   "Check network connectivity. Jina Reader is at https://r.jina.ai/.")
    if status == 401 or status == 403:
        emit_error("jina_fetch",
                   f"Jina Reader returned HTTP {status} (blocked / rate-limited)",
                   "r.jina.ai is a free public service. If persistently blocked, "
                   "sign up for an API key at https://jina.ai and prepend "
                   "'Authorization: Bearer <key>' via env JINA_API_KEY.",
                   exit_code=3)
    if status >= 400:
        emit_error("jina_fetch", f"Jina Reader HTTP {status}: {body[:200]}",
                   exit_code=3)

    # Parse JSON body
    try:
        data = json.loads(body)
    except json.JSONDecodeError as e:
        emit_error("jina_fetch", f"Jina returned non-JSON: {e}",
                   f"First 200 chars of body: {body[:200]!r}",
                   exit_code=3)

    # Jina JSON shape: {"data": {"content": "...md...", "title": "..."}, ...}
    inner = data.get("data") if isinstance(data, dict) else None
    if not isinstance(inner, dict):
        emit_error("jina_fetch",
                   "Jina response missing 'data' object",
                   hint=f"Body shape: {list(data.keys()) if isinstance(data, dict) else type(data).__name__}",
                   exit_code=3)

    title = inner.get("title") or ""
    content = inner.get("content") or ""
    description = inner.get("description") or ""
    truncated = False
    if len(content) > max_chars:
        content = content[:max_chars] + "\n\n[…truncated…]"
        truncated = True

    return {
        "ok": True,
        "command": "jina_fetch",
        "url": url,
        "title": title,
        "description": description,
        "content": content,
        "content_chars": len(content),
        "truncated": truncated,
        "channel": "jina",
    }


# ─────────────────────────────────────────────────────────────────────────────
# Channel: youtube — fetch metadata + subtitles for a YouTube URL via yt-dlp
# ─────────────────────────────────────────────────────────────────────────────
# yt-dlp comes as a dependency of agent-reach and is already installed in the
# venv. We invoke it as a subprocess to:
#   1) avoid reimplementing YouTube's player JS unwinding,
#   2) get automatic subtitle extraction (auto-generated captions work without
#      YouTube API keys).
#
# We deliberately do NOT download the video (`--skip-download`).
# We DO dump:
#   - metadata as JSON
#   - auto-generated subtitles (if any) as .vtt
#
# Output is parsed into a clean {title, channel, duration, subtitles} dict.

def _find_yt_dlp() -> str:
    """Locate the yt-dlp binary. The runner is launched from the venv's
    python.exe, so sys.executable's dirname IS the venv's Scripts/ dir.
    Returns absolute path or "".
    """
    import shutil
    p = shutil.which("yt-dlp")
    if p:
        return p
    exe = "yt-dlp.exe" if os.name == "nt" else "yt-dlp"
    cand = os.path.join(os.path.dirname(sys.executable), exe)
    return cand if os.path.isfile(cand) else ""


def cmd_youtube(params: Dict[str, Any]) -> Dict[str, Any]:
    url = (params.get("url") or "").strip()
    if not url:
        emit_error("youtube", "Missing 'url' parameter",
                   "Pass {\"command\": \"youtube\", \"url\": \"https://youtu.be/...\"}",
                   exit_code=2)
    if "youtu" not in url.lower():
        emit_error("youtube", f"URL does not look like a YouTube link: {url!r}",
                   exit_code=2)

    want_subs = bool(params.get("subtitles", True))
    max_chars = int(params.get("max_chars", 8000))

    ytdlp = _find_yt_dlp()
    if not ytdlp:
        emit_error("youtube",
                   "yt-dlp binary not found",
                   hint=("Run: python -m pip install -U yt-dlp   (or reinstall agent-reach)"),
                   exit_code=4)

    with tempfile.TemporaryDirectory(prefix="ar_yt_") as td:
        out_template = os.path.join(td, "%(id)s.%(ext)s")
        args = [
            ytdlp,
            "--skip-download",            # we never want the video file
            "--no-warnings",
            "--no-playlist",
            "--write-info-json",          # dump metadata
            "-o", out_template,
            url,
        ]
        if want_subs:
            # Ask for auto-generated English subs + plain text output
            args += [
                "--write-auto-subs",
                "--sub-langs", "en.*,en",
                "--sub-format", "vtt",
                "--convert-subs", "vtt",
            ]
        try:
            proc = subprocess.run(args, capture_output=True, text=True, timeout=TIMEOUT_YT)
        except subprocess.TimeoutExpired:
            emit_error("youtube", f"yt-dlp timed out after {TIMEOUT_YT}s", exit_code=3)
        if proc.returncode != 0:
            err = (proc.stderr or proc.stdout or "").strip()
            emit_error("youtube", f"yt-dlp failed (exit {proc.returncode}): {err[:500]}",
                       exit_code=3)

        # Find the .info.json file yt-dlp dropped
        info_path = ""
        for n in os.listdir(td):
            if n.endswith(".info.json"):
                info_path = os.path.join(td, n)
                break
        if not info_path:
            emit_error("youtube", "yt-dlp did not produce an info.json", exit_code=3)
        with open(info_path, "r", encoding="utf-8") as f:
            info = json.load(f)

        result: Dict[str, Any] = {
            "ok": True,
            "command": "youtube",
            "url": url,
            "id": info.get("id"),
            "title": info.get("title"),
            "channel": info.get("channel") or info.get("uploader"),
            "duration_seconds": info.get("duration"),
            "view_count": info.get("view_count"),
            "upload_date": info.get("upload_date"),
            "description": (info.get("description") or "")[:2000],
            "tags": (info.get("tags") or [])[:20],
            "subtitles": "",
        }

        if want_subs:
            # Find a .vtt file
            for n in os.listdir(td):
                if n.endswith(".vtt"):
                    with open(os.path.join(td, n), "r", encoding="utf-8", errors="replace") as f:
                        vtt = f.read()
                    # Strip VTT timestamps and WEBVTT header to get plain text
                    text_lines = []
                    for line in vtt.splitlines():
                        if line.startswith("WEBVTT"):
                            continue
                        if re.match(r"^\d{2}:\d{2}:\d{2}\.\d{3} -->", line):
                            continue
                        if line.strip() == "":
                            continue
                        # VTT cue timing tags: <00:00:00.000>
                        line = re.sub(r"<\d{2}:\d{2}:\d{2}\.\d{3}>", "", line)
                        text_lines.append(line.strip())
                    sub_text = " ".join(text_lines)
                    if len(sub_text) > max_chars:
                        sub_text = sub_text[:max_chars] + "…"
                    result["subtitles"] = sub_text
                    result["subtitles_chars"] = len(sub_text)
                    break
            else:
                result["subtitles_note"] = "No subtitles (auto-generated captions unavailable)"

        return result


# ─────────────────────────────────────────────────────────────────────────────
# Channel: bilibili — search Bilibili videos via the public search API
# ─────────────────────────────────────────────────────────────────────────────
# We call https://api.bilibili.com/x/web-interface/search/all/v2?keyword=...
# which is the JSON endpoint that the web search page uses.
# No auth required, but Bilibili aggressively rate-limits anonymous clients
# (HTTP 412 on rapid calls), so the Rust caller should treat 412 as retryable.

def cmd_bilibili(params: Dict[str, Any]) -> Dict[str, Any]:
    query = (params.get("query") or "").strip()
    if not query:
        emit_error("bilibili", "Missing 'query' parameter",
                   "Pass {\"command\": \"bilibili\", \"query\": \"...\"}",
                   exit_code=2)
    top_k = int(params.get("top_k", 5))
    if top_k < 1 or top_k > 20:
        top_k = 5

    # Mimic a browser — Bilibili returns 412 to plain urllib.
    url = "https://api.bilibili.com/x/web-interface/search/all/v2?" + urllib.parse.urlencode({
        "keyword": query,
        "page": 1,
        "pagesize": top_k,
    })
    status, _, body = _http_get(
        url, TIMEOUT_BILI,
        headers={
            "Referer": "https://www.bilibili.com/",
            "Accept": "application/json",
            "Accept-Language": "zh-CN,zh;q=0.9,en;q=0.8",
        },
    )
    if status == 412:
        emit_error("bilibili",
                   "Bilibili returned HTTP 412 (precondition failed / anti-bot)",
                   "Slow down: space calls at least 3s apart, or use cookie auth.",
                   exit_code=3)
    if status == 0 or status >= 400:
        emit_error("bilibili", f"Bilibili HTTP {status}: {body[:200]}", exit_code=3)

    try:
        data = json.loads(body)
    except json.JSONDecodeError as e:
        emit_error("bilibili", f"Non-JSON from Bilibili: {e}", exit_code=3)

    if data.get("code") not in (0, None):
        emit_error("bilibili",
                   f"Bilibili API error code={data.get('code')}: {data.get('message')}",
                   exit_code=3)

    items: List[Dict[str, Any]] = []
    for result_block in (data.get("data") or {}).get("result", []):
        for v in (result_block.get("data") or []):
            items.append({
                "title": re.sub(r"<[^>]+>", "", v.get("title", "")),
                "author": v.get("author", ""),
                "bvid": v.get("bvid", ""),
                "aid": v.get("aid"),
                "duration": v.get("duration", ""),
                "play": v.get("play", 0),
                "url": f"https://www.bilibili.com/video/{v.get('bvid', '')}" if v.get("bvid") else "",
            })
            if len(items) >= top_k:
                break
        if len(items) >= top_k:
            break

    return {
        "ok": True,
        "command": "bilibili",
        "query": query,
        "channel": "bilibili-search-api",
        "count": len(items),
        "items": items,
    }


# ─────────────────────────────────────────────────────────────────────────────
# Channel: rss — fetch and parse an RSS/Atom feed
# ─────────────────────────────────────────────────────────────────────────────
# Uses feedparser, which is a dependency of agent-reach.

def cmd_rss(params: Dict[str, Any]) -> Dict[str, Any]:
    feed_url = (params.get("url") or params.get("feed") or "").strip()
    if not feed_url:
        emit_error("rss", "Missing 'url' parameter (the feed URL)",
                   "Pass {\"command\": \"rss\", \"url\": \"https://example.com/feed.xml\"}",
                   exit_code=2)
    top_k = int(params.get("top_k", 10))
    if top_k < 1 or top_k > 50:
        top_k = 10

    try:
        import feedparser  # type: ignore
    except ImportError:
        emit_error("rss", "feedparser not installed in this Python environment",
                   hint="pip install feedparser   (it's a dep of agent-reach)",
                   exit_code=4)

    status, _, body = _http_get(feed_url, TIMEOUT_RSS,
                                headers={"Accept": "application/atom+xml, application/rss+xml, */*"})
    if status == 0 or status >= 400:
        emit_error("rss", f"Feed fetch HTTP {status}", exit_code=3)

    parsed = feedparser.parse(body)
    if parsed.bozo and not parsed.entries:
        emit_error("rss", f"Feed parse error: {parsed.bozo_exception}", exit_code=3)

    feed_title = (parsed.feed.get("title", "") if hasattr(parsed, "feed") else "")
    items: List[Dict[str, Any]] = []
    for e in parsed.entries[:top_k]:
        items.append({
            "title": e.get("title", ""),
            "link": e.get("link", ""),
            "published": e.get("published", "") or e.get("updated", ""),
            "summary": (e.get("summary", "") or e.get("description", ""))[:500],
        })
    return {
        "ok": True,
        "command": "rss",
        "url": feed_url,
        "feed_title": feed_title,
        "channel": "feedparser",
        "count": len(items),
        "items": items,
    }


# ─────────────────────────────────────────────────────────────────────────────
# Channel: github — fetch a README or raw file from a public repo
# ─────────────────────────────────────────────────────────────────────────────
# We do NOT depend on the `gh` CLI (which agent-reach flagged as missing).
# Instead we hit raw.githubusercontent.com directly, which serves any public
# file without auth. For README, we try common filenames in order.

GITHUB_README_CANDIDATES = [
    "README.md", "README.markdown", "README.rst", "README.txt", "README",
    "readme.md", "Readme.md",
]

def cmd_github(params: Dict[str, Any]) -> Dict[str, Any]:
    repo = (params.get("repo") or "").strip()  # "owner/name"
    if not repo or "/" not in repo:
        emit_error("github", "Missing or invalid 'repo' parameter",
                   'Pass {\"command\": \"github\", \"repo\": \"owner/name\"}',
                   exit_code=2)
    branch = (params.get("branch") or "HEAD").strip()
    path = (params.get("path") or "").strip().lstrip("/")

    if path:
        # Direct raw file
        url = f"https://raw.githubusercontent.com/{repo}/{branch}/{path}"
        status, _, body = _http_get(url, TIMEOUT_GH)
        if status == 0 or status >= 400:
            emit_error("github", f"GitHub raw fetch HTTP {status} for {url}", exit_code=3)
        return {
            "ok": True,
            "command": "github",
            "channel": "raw.githubusercontent.com",
            "repo": repo,
            "branch": branch,
            "path": path,
            "content": body,
            "content_chars": len(body),
        }

    # No path → look for README on default branch
    # First resolve the default branch via the GitHub API
    api_url = f"https://api.github.com/repos/{repo}"
    status, _, body = _http_get(api_url, TIMEOUT_GH,
                                headers={"Accept": "application/vnd.github+json"})
    if status == 0 or status >= 400:
        emit_error("github", f"GitHub repo metadata HTTP {status} for {repo}",
                   "If the repo is private, set GITHUB_TOKEN in env and add an Authorization header upstream.",
                   exit_code=3)
    try:
        meta = json.loads(body)
    except json.JSONDecodeError as e:
        emit_error("github", f"GitHub API non-JSON: {e}", exit_code=3)
    default_branch = meta.get("default_branch", "main")

    for cand in GITHUB_README_CANDIDATES:
        url = f"https://raw.githubusercontent.com/{repo}/{default_branch}/{cand}"
        status, _, body = _http_get(url, TIMEOUT_GH)
        if status == 200:
            return {
                "ok": True,
                "command": "github",
                "channel": "raw.githubusercontent.com",
                "repo": repo,
                "branch": default_branch,
                "path": cand,
                "content": body,
                "content_chars": len(body),
            }
    emit_error("github",
               f"No README found for {repo} on branch {default_branch}",
               "Pass an explicit 'path' (e.g. 'docs/index.md') to fetch another file.",
               exit_code=2)


# ─────────────────────────────────────────────────────────────────────────────
# Channel: doctor — return the Agent-Reach doctor report as JSON
# ─────────────────────────────────────────────────────────────────────────────
# We call the installed `agent-reach` CLI's `doctor` subcommand. This is
# informational — useful for the LLM to self-diagnose ("is YouTube working?").

def _find_agent_reach_cli() -> str:
    """Locate the `agent-reach` console script. The runner is launched with
    the venv's python.exe as sys.executable — the dirname of sys.executable
    is ALREADY the venv's Scripts/ (or bin/) directory on Windows. So we just
    join the exe name directly. As a secondary check we fall back to PATH.

    Returns "" if not found.
    """
    exe = "agent-reach.exe" if os.name == "nt" else "agent-reach"
    side_by_side = os.path.join(os.path.dirname(sys.executable), exe)
    if os.path.isfile(side_by_side):
        return side_by_side
    import shutil
    p = shutil.which("agent-reach")
    return p or ""


def cmd_doctor(params: Dict[str, Any]) -> Dict[str, Any]:
    cli = _find_agent_reach_cli()
    if not cli:
        emit_error("doctor", "agent-reach CLI not found",
                   "pip install agent-reach into the venv this runner is launched from",
                   exit_code=4)
    env = os.environ.copy()
    env.setdefault("PYTHONIOENCODING", "utf-8")
    env.setdefault("NO_COLOR", "1")
    env.setdefault("TERM", "dumb")
    try:
        # text=False + manual decode (errors=replace) — the CLI emits rich-progress
        # box-drawing bytes that crash Windows cp1252 decoding in the stdlib reader.
        proc = subprocess.run(
            [cli, "doctor"],
            capture_output=True,        # bytes
            text=False,
            timeout=60, env=env,
        )
        # agent-reach writes the report to stdout, but some warnings leak to stderr.
        out = proc.stdout.decode("utf-8", errors="replace")
        err = proc.stderr.decode("utf-8", errors="replace")
        report = out if out.strip() else err
    except subprocess.TimeoutExpired:
        emit_error("doctor", "agent-reach doctor timed out after 60s", exit_code=3)
    return {
        "ok": proc.returncode == 0,
        "command": "doctor",
        "channel": "agent-reach CLI",
        "returncode": proc.returncode,
        "report": out,
    }


# ─────────────────────────────────────────────────────────────────────────────
# Dispatch
# ─────────────────────────────────────────────────────────────────────────────

COMMANDS = {
    "jina_fetch": cmd_jina_fetch,
    "youtube": cmd_youtube,
    "bilibili": cmd_bilibili,
    "rss": cmd_rss,
    "github": cmd_github,
    "doctor": cmd_doctor,
}


def main() -> int:
    raw = sys.stdin.read()
    if not raw.strip():
        emit_error("unknown", "Empty stdin — expected JSON params",
                   'Pass {"command": "jina_fetch", "url": "..."}', exit_code=2)
    try:
        params = json.loads(raw)
    except json.JSONDecodeError as e:
        emit_error("unknown", f"Invalid JSON on stdin: {e}",
                   f"First 200 chars: {raw[:200]!r}", exit_code=2)
    if not isinstance(params, dict):
        emit_error("unknown", "Stdin JSON must be an object", exit_code=2)

    command = (params.get("command") or "").strip()
    if not command:
        emit_error("unknown", "Missing 'command' field",
                   f"Supported: {sorted(COMMANDS)}", exit_code=2)
    handler = COMMANDS.get(command)
    if handler is None:
        emit_error(command, f"Unknown command: {command!r}",
                   f"Supported: {sorted(COMMANDS)}", exit_code=2)

    try:
        result = handler(params)
    except SystemExit:
        raise  # emit_error already wrote and exited
    except Exception as e:
        emit_error(command, f"{type(e).__name__}: {e}", exit_code=3)

    emit(result)
    return 0


if __name__ == "__main__":
    sys.exit(main())
