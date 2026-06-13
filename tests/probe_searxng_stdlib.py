"""
probe_searxng_stdlib.py — Probe public SearXNG instances using Python stdlib only.

No external deps. Tests 30 well-known public instances with a realistic
Firefox 128 User-Agent and JSON Accept header. Reports per-instance
status and prints a recommended `set SEARXNG_BASE_URL=...` line for the
first one that returns a non-empty result list.

Usage:
    python tests/probe_searxng_stdlib.py
"""
from __future__ import annotations

import json
import socket
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed

INSTANCES = [
    "https://searx.be",
    "https://search.disroot.org",
    "https://searx.tiekoetter.com",
    "https://paulgo.io",
    "https://searx.foss.wtf",
    "https://search.bus-hit.me",
    "https://search.sapti.me",
    "https://searx.namejeff.xyz",
    "https://search.namejeff.xyz",
    "https://searx.hu",
    "https://searx.work",
    "https://searx.lavatech.top",
    "https://search.smnz.de",
    "https://searxng.nicfab.eu",
    "https://searx.prvcy.eu",
    "https://search.inetol.net",
    "https://search.canine.tools",
    "https://priv.au",
    "https://searx.jakelee.co.uk",
    "https://searx.aleteoryx.me",
    "https://searx.win",
    "https://searx.oakleycord.dev",
    "https://searx.dnswarden.com",
    "https://search.gcomm.ch",
    "https://searxng.world",
    "https://searxng.online",
    "https://searx.zackptg5.com",
    "https://search.mdosch.de",
    "https://searxng.ninja",
    "https://searx.sp-codes.de",
]

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) "
      "Gecko/20100101 Firefox/128.0")
TIMEOUT = 8.0
QUERY = "rust async runtime 2025"


def make_ssl_ctx() -> ssl.SSLContext:
    ctx = ssl.create_default_context()
    # Most public instances use valid certs; keep verify on by default.
    return ctx


def probe(base: str) -> tuple[str, str, int | None, int]:
    """Probe one instance. Returns (base, status_label, http_code_or_none, num_results)."""
    url = f"{base}/search?{urllib.parse.urlencode({'q': QUERY, 'format': 'json', 'categories': 'general', 'safesearch': 0, 'language': 'en'})}"
    req = urllib.request.Request(
        url,
        headers={
            "User-Agent": UA,
            "Accept": "application/json, text/html;q=0.9, */*;q=0.8",
            "Accept-Language": "en-US,en;q=0.5",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT, context=make_ssl_ctx()) as resp:
            code = resp.getcode()
            body = resp.read(200_000)
    except urllib.error.HTTPError as e:
        return (base, f"HTTP_{e.code}", e.code, 0)
    except urllib.error.URLError as e:
        reason = e.reason if hasattr(e, "reason") else str(e)
        if isinstance(reason, ssl.SSLError):
            return (base, "SSL", None, 0)
        if isinstance(reason, socket.timeout) or "timed out" in str(reason).lower():
            return (base, "TIMEOUT", None, 0)
        if "getaddrinfo" in str(reason).lower() or "name resolution" in str(reason).lower():
            return (base, "DNS", None, 0)
        if "connection refused" in str(reason).lower():
            return (base, "CONN_REFUSED", None, 0)
        return (base, f"URL_{type(reason).__name__}", None, 0)
    except socket.timeout:
        return (base, "TIMEOUT", None, 0)
    except (ConnectionError, ConnectionResetError, ConnectionAbortedError) as e:
        return (base, f"CONN_{type(e).__name__}", None, 0)
    except OSError as e:
        return (base, f"OS_{e.__class__.__name__}", None, 0)
    except Exception as e:
        return (base, f"ERR_{type(e).__name__}", None, 0)

    # Try parse JSON
    try:
        data = json.loads(body.decode("utf-8", errors="replace"))
    except (ValueError, UnicodeDecodeError):
        return (base, "PARSE_FAIL", code, 0)

    results = data.get("results") or []
    n = len(results)
    if n == 0:
        return (base, "EMPTY", code, 0)
    return (base, "OK", code, n)


def main() -> int:
    print(f"Probing {len(INSTANCES)} public SearXNG instances…")
    print(f"Query: {QUERY!r}   Timeout: {TIMEOUT}s   UA: {UA!r}\n")

    t0 = time.time()
    statuses: dict[str, int] = {}
    rows: list[tuple[str, str, int | None, int]] = []
    with ThreadPoolExecutor(max_workers=8) as ex:
        futs = {ex.submit(probe, base): base for base in INSTANCES}
        for fut in as_completed(futs):
            base, label, code, n = fut.result()
            rows.append((base, label, code, n))
            statuses[label] = statuses.get(label, 0) + 1

    # Sort: OK first (by result count desc), then HTTP/URL/other
    def keyfn(r):
        _base, label, _code, n = r
        return (0 if label == "OK" else 1, -n, _base)

    rows.sort(key=keyfn)
    elapsed = time.time() - t0

    print(f"{'Instance':<40} {'Status':<14} {'Code':<6} {'Results'}")
    print("-" * 78)
    for base, label, code, n in rows:
        code_str = str(code) if code is not None else "-"
        print(f"{base:<40} {label:<14} {code_str:<6} {n}")

    print("-" * 78)
    print(f"Summary ({elapsed:.1f}s): " + ", ".join(
        f"{k}={v}" for k, v in sorted(statuses.items(), key=lambda kv: -kv[1])
    ))

    ok_rows = [r for r in rows if r[1] == "OK"]
    if ok_rows:
        first = ok_rows[0]
        print()
        print(f"FIRST OK: {first[0]}  (results={first[3]})")
        print(f"Suggested:  set SEARXNG_BASE_URL={first[0]}")
    else:
        print()
        print("No working public instance found. Consider self-hosting via Docker:")
        print("  docker run -d --name searxng -p 8888:8080 \\")
        print("    -e SEARXNG_SECRET=changeme searxng/searxng")
        print("Then:  set SEARXNG_BASE_URL=http://localhost:8888")
    return 0 if ok_rows else 1


if __name__ == "__main__":
    sys.exit(main())
