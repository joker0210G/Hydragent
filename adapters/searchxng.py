"""
searchxng.py — Tiny SearXNG-compatible HTTP service backed by `ddgs`.

Exposes a single endpoint:

    GET /search?q=<query>&format=json&categories=general&safesearch=0&language=en

Returns a SearXNG-shaped JSON payload:

    {
      "query": "...",
      "number_of_results": N,
      "results": [
        {"title": "...", "url": "...", "content": "...", "engine": "ddgs"},
        ...
      ],
      "answers": [],
      "infoboxes": [],
      "suggestions": [],
      "unresponsive_engines": []
    }

This lets the existing Rust `web_search` tool (which already speaks
SearXNG JSON) point at this local service via:

    set SEARXNG_BASE_URL=http://localhost:7777

Usage:
    python adapters/searchxng.py [--host 127.0.0.1] [--port 7777]

Dependencies (already in adapters/.venv):
    ddgs 9.14+   (pip install ddgs)
"""
from __future__ import annotations

import argparse
import http.server
import json
import logging
import socketserver
import sys
import time
from typing import Any
from urllib.parse import parse_qs, urlparse

try:
    from ddgs import DDGS
except ImportError as e:
    print(
        f"FATAL: ddgs is not installed. Run:\n"
        f"    adapters\\.venv\\Scripts\\python.exe -m pip install ddgs\n"
        f"Original error: {e}",
        file=sys.stderr,
    )
    sys.exit(2)


# ---------------------------------------------------------------------------
# SearXNG-shape transformer
# ---------------------------------------------------------------------------

def ddgs_to_searxng(query: str, raw_results: list[dict[str, Any]],
                    language: str = "en") -> dict[str, Any]:
    """Convert `ddgs.text()` output to a SearXNG-shaped JSON payload."""
    results: list[dict[str, Any]] = []
    for r in raw_results:
        title = r.get("title") or ""
        href = r.get("href") or r.get("url") or ""
        body = r.get("body") or r.get("content") or ""
        if not href:
            continue
        results.append(
            {
                "title": title,
                "url": href,
                "content": body,
                "engine": "ddgs",
                "engines": ["ddgs"],
                "score": 1.0,
                "category": "general",
                "language": language or "en",
            }
        )

    return {
        "query": query,
        "number_of_results": len(results),
        "results": results,
        "answers": [],
        "infoboxes": [],
        "suggestions": [],
        "unresponsive_engines": [],
    }


# ---------------------------------------------------------------------------
# HTTP handler
# ---------------------------------------------------------------------------

class SearchXNGHandler(http.server.BaseHTTPRequestHandler):
    server_version = "SearchXNG/1.0 (ddgs-backed SearXNG shim)"

    # ---------- helpers ----------
    def _write_json(self, status: int, payload: dict[str, Any] | list[Any]) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _write_error(self, status: int, msg: str) -> None:
        self._write_json(status, {"error": msg, "results": []})

    def log_message(self, fmt: str, *args: Any) -> None:
        # Route through logging so we get timestamps and the module logger
        logging.info("%s - - %s", self.address_string(), fmt % args)

    # ---------- routes ----------
    def do_GET(self) -> None:  # noqa: N802 (BaseHTTPRequestHandler)
        parsed = urlparse(self.path)
        path = parsed.path.rstrip("/") or "/"

        if path == "/" or path == "/healthz":
            self._write_json(200, {
                "status": "ok",
                "service": "searchxng",
                "backend": "ddgs",
                "endpoint": "/search?q=<query>&format=json",
            })
            return

        if path != "/search":
            self._write_error(404, f"not found: {path}")
            return

        qs = parse_qs(parsed.query)
        query = (qs.get("q") or qs.get("query") or [""])[0].strip()
        if not query:
            self._write_error(400, "missing required query parameter 'q'")
            return

        fmt = (qs.get("format") or ["json"])[0].lower()
        if fmt != "json":
            self._write_error(
                415,
                f"only format=json is supported (got format={fmt!r})",
            )
            return

        try:
            max_results = int((qs.get("max_results") or ["10"])[0])
        except ValueError:
            max_results = 10
        max_results = max(1, min(max_results, 30))

        safesearch_raw = (qs.get("safesearch") or ["0"])[0]
        try:
            safesearch = int(safesearch_raw)
        except ValueError:
            safesearch = 0
        # ddgs uses 'on' / 'moderate' / 'off' — map from int 0/1/2
        safesearch_label = {0: "off", 1: "moderate", 2: "on"}.get(safesearch, "off")

        language = (qs.get("language") or ["en"])[0]
        region = (qs.get("region") or ["wt-wt"])[0]
        # ddgs uses 2-letter codes; "en-US" → "us-en" in some versions, so keep
        # language only and let ddgs pick region from it
        if "-" in language:
            language = language.split("-")[0]
        if language.lower() in ("all", "wt", "wt-wt"):
            language = "en"  # ddgs requires a real lang; fall back to English

        t0 = time.time()
        try:
            with DDGS() as ddgs:
                raw = list(
                    ddgs.text(
                        query=query,
                        region=region,
                        safesearch=safesearch_label,
                        timelimit=None,
                        max_results=max_results,
                        backend="auto",
                    )
                )
        except Exception as e:  # noqa: BLE001
            logging.exception("ddgs search failed")
            self._write_error(502, f"ddgs backend error: {e!s}")
            return

        payload = ddgs_to_searxng(query, raw, language=language)
        payload["search"] = {
            "query": query,
            "max_results": max_results,
            "safesearch": safesearch,
            "language": language,
            "elapsed_seconds": round(time.time() - t0, 3),
            "backend": "ddgs",
        }
        self._write_json(200, payload)


# ---------------------------------------------------------------------------
# entry point
# ---------------------------------------------------------------------------

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="SearXNG-compatible HTTP shim backed by ddgs.",
    )
    parser.add_argument("--host", default="127.0.0.1",
                        help="bind host (default: 127.0.0.1)")
    parser.add_argument("--port", type=int, default=7777,
                        help="bind port (default: 7777)")
    parser.add_argument("--log-level", default="INFO",
                        help="logging level (default: INFO)")
    args = parser.parse_args(argv)

    logging.basicConfig(
        level=args.log_level.upper(),
        format="%(asctime)s %(levelname)s searchxng: %(message)s",
    )

    # Reuse the listening socket across requests (ThreadingTCPServer is overkill
    # for a local single-user tool, but ThreadingTCPServer is a tiny bit nicer
    # if a swarm fans out parallel web_search calls).
    class ThreadingServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
        allow_reuse_address = True
        daemon_threads = True

    with ThreadingServer((args.host, args.port), SearchXNGHandler) as srv:
        logging.info("listening on http://%s:%d  (try /healthz)", args.host, args.port)
        try:
            srv.serve_forever()
        except KeyboardInterrupt:
            logging.info("shutting down")
    return 0


if __name__ == "__main__":
    sys.exit(main())
