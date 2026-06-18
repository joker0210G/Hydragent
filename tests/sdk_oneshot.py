#!/usr/bin/env python3
"""End-to-end SDK smoke test against a live `hydragent` kernel on :5000.

Run:
    python tests/sdk_oneshot.py

Exits 0 on success, non-zero on failure. Prints the full LLM response
on success.
"""
from __future__ import annotations
import sys
from pathlib import Path

# Add the `adapters/` directory to sys.path so we can import the
# in-tree `hydragent_py` package without requiring a `pip install`.
_THIS_DIR = Path(__file__).resolve().parent
_ADAPTERS = _THIS_DIR.parent / "adapters"
if str(_ADAPTERS) not in sys.path:
    sys.path.insert(0, str(_ADAPTERS))

from hydragent_py import HydraClient, HydraConfig, HydraError  # type: ignore[import-not-found]


def main() -> int:
    cfg = HydraConfig.from_env()
    print(f"[1/4] Connecting to kernel at {cfg.bus_host}:{cfg.bus_port} ...")
    try:
        client = HydraClient.connect(cfg)
    except HydraError as e:
        print(f"  ✗ HydraError: {e}")
        return 1
    except Exception as e:
        print(f"  ✗ Unexpected: {type(e).__name__}: {e}")
        return 2

    print(f"[2/4] Connected. page_id={client.config.page_id}")
    prompt = "Reply with exactly: PONG. Nothing else."
    print(f"[3/4] Sending: {prompt!r}")
    try:
        answer = client.chat(prompt)
    except HydraError as e:
        print(f"  ✗ HydraError: {e}")
        client.close()
        return 3
    except Exception as e:
        print(f"  ✗ Unexpected: {type(e).__name__}: {e}")
        client.close()
        return 4

    print(f"[4/4] LLM answered: {answer!r}")
    client.close()
    if "PONG" in answer.upper():
        print("\n[OK] SDK -> bus -> kernel -> LLM round-trip OK")
        return 0
    else:
        print(f"\n[FAIL] Unexpected answer (expected 'PONG'): {answer!r}")
        return 5


if __name__ == "__main__":
    sys.exit(main())
