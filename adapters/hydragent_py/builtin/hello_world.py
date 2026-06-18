# Bundled plugin: hello_world
#
# This is the canonical "first plugin" example. It registers a
# single `hello` tool that always returns a friendly greeting.
#
# To enable, ensure the `hydragent_py` package is on `sys.path` and
# that `~/.hydragent/plugins/` (or any of the other discovery
# locations) contains a symlink or copy of this file. The bundled
# copy under `hydragent_py/builtin/` is always loaded.

from hydragent_py.plugins import PluginContext


def register(ctx: PluginContext) -> None:
    """Add a `hello` tool to the registry."""
    ctx.add_tool(
        name="hello",
        description="Print a friendly greeting.",
        parameters={"type": "object", "properties": {}, "additionalProperties": False},
        handler=lambda **_: "hello, world 🐉",
        permission_tier="AutoApprove",
    )
