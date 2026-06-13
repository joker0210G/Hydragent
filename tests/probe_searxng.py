#!/usr/bin/env python3
"""
SearXNG instance probe — find one that works from this network.
Run: python tests/probe_searxng.py
"""
import requests
import sys
import time

# Public SearXNG instances, ordered by reliability
INSTANCES = [
    "https://searx.be",
    "https://search.disroot.org",
    "https://searx.tiekoetter.com",
    "https://paulgo.io",
    "https://baresearch.org",
    "https://search.mdosch.de",
    "https://searx.prvcy.eu",
    "https://searx.namejeff.xyz",
    "https://etsi.me",
    "https://search.foobar.tech",
    "https://searx.work",
    "https://searx.lavatech.top",
    "https://search.canine.tools",
    "https://search.sapti.me",
    "https://searx.aleteoryx.me",
    "https://opnxng.com",
    "https://searxng.nicfab.eu",
    "https://search.in.projectsegfau.lt",
    "https://searx.kabi.tk",
    "https://searx.mastodonte.com",
    "https://searx.headpat.exchange",
    "https://priv.au",
    "https://searx.hu",
    "https://searx.oakleycord.dev",
    "https://s.zhaocloud.net",
    "https://searx.privatedns.org",
    "https://searx.tux.pizza",
    "https://search.neatia.xyz",
    "https://searx.colbster937.dev",
    "https://paulgo-ca.iqth.host",
]

UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 Firefox/128.0"

def probe(url: str) -> tuple[str, str]:
    """Return (status, info) for a single instance."""
    try:
        r = requests.get(
            url + "/search?q=test&format=json",
            headers={"User-Agent": UA, "Accept": "application/json"},
            timeout=10,
        )
        if r.status_code != 200:
            return f"FAIL  HTTP {r.status_code}", ""
        try:
            data = r.json()
        except Exception as e:
            return f"FAIL  non-JSON: {str(e)[:40]}", ""
        results = data.get("results", [])
        if not results:
            return "EMPTY 0 results", ""
        first = results[0]
        title = first.get("title", "?")[:60]
        engine = first.get("engine", "?")
        return f"OK    {len(results):>2} results", f"  first={title!r} (engine={engine})"
    except requests.exceptions.Timeout:
        return "TIMEOUT", ""
    except requests.exceptions.SSLError as e:
        return f"SSL    {str(e)[:40]}", ""
    except requests.exceptions.ConnectionError as e:
        return f"CONN   {str(e)[:40]}", ""
    except Exception as e:
        return f"ERR    {type(e).__name__}: {str(e)[:40]}", ""


def main() -> int:
    print(f"Probing {len(INSTANCES)} public SearXNG instances...")
    print(f"User-Agent: {UA}\n")
    working = []
    for url in INSTANCES:
        status, info = probe(url)
        marker = "★" if status.startswith("OK") else " "
        print(f"  {marker} {url:45s} {status}{info}")
        if status.startswith("OK"):
            working.append(url)
        time.sleep(0.3)  # be polite

    print()
    if working:
        print(f"{len(working)}/{len(INSTANCES)} instances working.")
        print()
        print("To use one, set the env var before starting the bus:")
        print(f'  set "SEARXNG_BASE_URL={working[0]}"')
        print()
        print("Or self-host SearXNG (no rate limits, instant):")
        print("  docker run -d --name searxng -p 8888:8080 \\")
        print("    -e SEARXNG_SECRET=changeme \\")
        print("    -e SEARXNG_PUBLIC_INSTANCE=false \\")
        print("    searxng/searxng")
        print("  set SEARXNG_BASE_URL=http://localhost:8888")
        return 0
    else:
        print("No public SearXNG instance is reachable from this network.")
        print("You'll need to self-host or use a different search backend.")
        return 1


if __name__ == "__main__":
    sys.exit(main())
