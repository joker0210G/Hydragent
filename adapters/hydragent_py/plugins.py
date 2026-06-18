# hydragent_py.plugins — Plugin discovery and loading.
#
# A Hydragent plugin is a Python file in one of the well-known plugin
# directories. At REPL (or notebook) startup, every `*.py` file is
# imported and given a chance to register itself by exposing a
# top-level `register()` callable.
#
# The plugin contract is intentionally minimal so that simple plugins
# stay one file and one function:
#
#     # ~/.hydragent/plugins/hello_world.py
#
#     def register(ctx):
#         """Hook into the REPL or augment the tool registry.
#
#         `ctx` is a `hydragent_py.plugins.PluginContext`. Plugins may
#         call any of its methods to add tools, intercept messages,
#         customise the prompt, or register slash commands.
#         """
#         ctx.add_tool(
#             name="hello",
#             description="Print a friendly greeting.",
#             parameters={"type": "object", "properties": {}},
#             handler=lambda **_: "hello, world",
#         )
#
# Plugin discovery locations (highest priority first):
#
#   1. `HYDRA_PLUGINS_DIR` environment variable, if set and pointing
#      to an existing directory.
#   2. `$HYDRA_DATA_DIR/plugins/` (defaults to `data/plugins/`).
#   3. `~/.hydragent/plugins/` (user-wide).
#   4. The `<hydragent_py>/plugins/builtin/` directory (bundled).
#
# Files whose name starts with `_` are ignored. A failure to import a
# plugin is logged and skipped (one broken plugin must not block the
# others).

from __future__ import annotations

import importlib.util
import logging
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional

log = logging.getLogger("hydragent.plugins")


@dataclass
class ToolSpec:
    """Description of a tool a plugin wants to register.

    This mirrors the JSON Schema that the Rust kernel expects, so the
    SDK can forward it through the `tools/register` bus method
    without further translation.
    """

    name: str
    description: str
    parameters: Dict[str, Any]
    handler: Callable[..., Any]
    permission_tier: str = "Prompt"  # AutoApprove | Prompt | Deny


@dataclass
class SlashCommand:
    name: str
    help_text: str
    handler: Callable[["PluginContext", str], None]


@dataclass
class PluginContext:
    """Per-REPL bag of state that plugins may extend."""

    tools: List[ToolSpec] = field(default_factory=list)
    slash_commands: List[SlashCommand] = field(default_factory=list)
    pre_send_hooks: List[Callable[[str], str]] = field(default_factory=list)
    post_receive_hooks: List[Callable[[str], str]] = field(default_factory=list)
    config: Dict[str, Any] = field(default_factory=dict)

    def add_tool(
        self,
        name: str,
        description: str,
        parameters: Dict[str, Any],
        handler: Callable[..., Any],
        permission_tier: str = "Prompt",
    ) -> None:
        self.tools.append(
            ToolSpec(
                name=name,
                description=description,
                parameters=parameters,
                handler=handler,
                permission_tier=permission_tier,
            )
        )

    def add_slash_command(self, name: str, help_text: str, handler: Callable[["PluginContext", str], None]) -> None:
        self.slash_commands.append(SlashCommand(name=name, help_text=help_text, handler=handler))

    def on_pre_send(self, hook: Callable[[str], str]) -> None:
        """Register a hook that mutates the user message before it is sent."""
        self.pre_send_hooks.append(hook)

    def on_post_receive(self, hook: Callable[[str], str]) -> None:
        """Register a hook that mutates the assistant reply before it is rendered."""
        self.post_receive_hooks.append(hook)


def _candidate_plugin_dirs() -> List[Path]:
    """Return the ordered list of plugin directories to scan."""
    candidates: List[Path] = []
    env_dir = os.getenv("HYDRA_PLUGINS_DIR")
    if env_dir and Path(env_dir).is_dir():
        candidates.append(Path(env_dir))

    data_dir = os.getenv("HYDRA_DATA_DIR", "data")
    data_plugins = Path(data_dir) / "plugins"
    if data_plugins.is_dir():
        candidates.append(data_plugins)

    home = Path.home() / ".hydragent" / "plugins"
    if home.is_dir():
        candidates.append(home)

    bundled = Path(__file__).parent / "builtin"
    if bundled.is_dir():
        candidates.append(bundled)

    return candidates


def discover() -> List[Path]:
    """Return the ordered list of plugin files found on disk."""
    files: List[Path] = []
    for d in _candidate_plugin_dirs():
        for p in sorted(d.glob("*.py")):
            if p.name.startswith("_"):
                continue
            files.append(p)
    return files


def load_all(ctx: Optional[PluginContext] = None) -> PluginContext:
    """Discover and load every plugin into `ctx` (or a fresh context)."""
    ctx = ctx or PluginContext()
    for path in discover():
        try:
            _load_one(path, ctx)
        except Exception as exc:  # noqa: BLE001
            log.warning("plugin %s failed to load: %s", path, exc)
    return ctx


def _load_one(path: Path, ctx: PluginContext) -> None:
    spec = importlib.util.spec_from_file_location(f"hydragent_plugin_{path.stem}", path)
    if spec is None or spec.loader is None:
        log.warning("plugin %s: could not build import spec", path)
        return
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    register = getattr(module, "register", None)
    if register is None:
        log.info("plugin %s: no `register` callable — skipping", path.name)
        return
    register(ctx)
    log.info("plugin %s: loaded", path.name)
