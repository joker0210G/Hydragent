#!/usr/bin/env python3
"""cli_adapter.py — Backwards-compatible shim.

This script used to contain the entire Rich-based REPL. As of v0.7.1,
the REPL has been refactored into the `hydragent_py` SDK package so
that it can be embedded in notebooks, plugins, and custom frontends.

This file is kept so that existing user invocations continue to work
unchanged:

    $ python adapters/cli_adapter.py
    $ python adapters/cli_adapter.py --page my-session

All of the actual implementation now lives in
`adapters/hydragent_py/repl.py`. The two scripts are behaviourally
identical; the shim just forwards the call.
"""
import os
import sys
# Inject paths for parent directory (adapters) and utils/
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "../..")))
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "../../utils")))

# Make the SDK importable when this file is run directly from
# `adapters/`, without requiring a `pip install -e .` first.
_THIS_DIR = os.path.dirname(os.path.abspath(__file__))
if _THIS_DIR not in sys.path:
    sys.path.insert(0, _THIS_DIR)

from hydragent_py.repl import run_repl  # noqa: E402


if __name__ == "__main__":
    sys.exit(run_repl())
