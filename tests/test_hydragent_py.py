#!/usr/bin/env python3
"""Smoke tests for the `hydragent_py` SDK.

Run:
    python -m pytest tests/test_hydragent_py.py -v
or
    python tests/test_hydragent_py.py

These tests don't talk to a real Hydragent bus — they exercise
the SDK's pure-Python surface: config defaults, plugin discovery,
import paths, and the legacy shim. End-to-end tests live in
`tests/test_ws_push_e2e.py`.
"""
from __future__ import annotations

import importlib
import importlib.util
import sys
from pathlib import Path

# Add the `adapters/` directory to sys.path so we can import the
# in-tree `hydragent_py` package without requiring a `pip install`.
_THIS_DIR = Path(__file__).resolve().parent
_ADAPTERS = _THIS_DIR.parent / "adapters"
if str(_ADAPTERS) not in sys.path:
    sys.path.insert(0, str(_ADAPTERS))


def test_top_level_imports() -> None:
    """The package exports the documented top-level symbols."""
    from hydragent_py import (  # type: ignore[import-not-found]
        BusClient,
        HydraClient,
        HydraConfig,
        HydraError,
        plugins,
    )
    assert HydraClient is not None
    assert HydraConfig is not None
    assert HydraError is not None
    assert BusClient is not None
    assert plugins is not None


def test_hydra_config_defaults() -> None:
    """HydraConfig defaults to a localhost client and a UUID page."""
    from hydragent_py import HydraConfig  # type: ignore[import-not-found]

    cfg = HydraConfig()
    assert cfg.bus_host == "127.0.0.1"
    assert cfg.bus_port == 5000
    # page_id is a uuid-prefixed string of length 36
    assert isinstance(cfg.page_id, str)
    assert len(cfg.page_id) == 36
    assert cfg.channel_id == "cli:default"


def test_hydra_config_from_env() -> None:
    """HydraConfig.from_env() honors HYDRA_BUS_HOST / HYDRA_BUS_PORT."""
    from hydragent_py import HydraConfig  # type: ignore[import-not-found]

    import os
    saved_host = os.environ.pop("HYDRA_BUS_HOST", None)
    saved_port = os.environ.pop("HYDRA_BUS_PORT", None)
    try:
        os.environ["HYDRA_BUS_HOST"] = "10.0.0.42"
        os.environ["HYDRA_BUS_PORT"] = "7777"
        cfg = HydraConfig.from_env()
        assert cfg.bus_host == "10.0.0.42"
        assert cfg.bus_port == 7777
    finally:
        if saved_host is not None:
            os.environ["HYDRA_BUS_HOST"] = saved_host
        else:
            os.environ.pop("HYDRA_BUS_HOST", None)
        if saved_port is not None:
            os.environ["HYDRA_BUS_PORT"] = saved_port
        else:
            os.environ.pop("HYDRA_BUS_PORT", None)


def test_bus_client_defaults() -> None:
    """BusClient defaults to 127.0.0.1:5000 — the local kernel bus."""
    from hydragent_py import BusClient  # type: ignore[import-not-found]

    client = BusClient()
    assert client.host == "127.0.0.1"
    assert client.port == 5000
    # The client should not have connected until .connect() is called.
    assert client.reader is None
    assert client.writer is None


def test_plugin_discovery_finds_builtin() -> None:
    """plugins.discover() should find `builtin/hello_world.py`."""
    from hydragent_py import plugins  # type: ignore[import-not-found]

    found = [p.name for p in plugins.discover()]
    assert "hello_world.py" in found, (
        f"hello_world.py should be in {found}"
    )


def test_legacy_shim() -> None:
    """`from bus_client import BusClient` still works (backwards-compat)."""
    sys.path.insert(0, str(_ADAPTERS))
    # Remove any cached modules so we re-import the shim.
    for name in ("bus_client",):
        sys.modules.pop(name, None)
    bus_client = importlib.import_module("bus_client")
    BusClient = bus_client.BusClient
    # The shim should be re-exporting the SDK's BusClient.
    from hydragent_py import BusClient as SdkBusClient
    assert BusClient is SdkBusClient


def test_cli_adapter_shim() -> None:
    """`adapters/cli_adapter.py` exists and is a thin shim (≤ 60 lines)."""
    shim = _ADAPTERS / "cli_adapter.py"
    assert shim.exists(), f"missing {shim}"
    line_count = sum(1 for _ in shim.open(encoding="utf-8"))
    assert line_count <= 60, (
        f"cli_adapter.py is {line_count} lines; should be a thin shim (<= 60)"
    )


# Allow running this file directly as a script (no pytest required).
if __name__ == "__main__":
    failures = 0
    tests = [
        test_top_level_imports,
        test_hydra_config_defaults,
        test_hydra_config_from_env,
        test_bus_client_defaults,
        test_plugin_discovery_finds_builtin,
        test_legacy_shim,
        test_cli_adapter_shim,
    ]
    for t in tests:
        try:
            t()
        except Exception as e:
            failures += 1
            print(f"  ✗ {t.__name__}: {e}")
        else:
            print(f"  ✓ {t.__name__}")
    if failures:
        print(f"\n{failures} test(s) FAILED")
        sys.exit(1)
    else:
        print(f"\nAll {len(tests)} tests PASSED")
