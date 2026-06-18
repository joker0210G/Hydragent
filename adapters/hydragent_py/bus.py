# hydragent_py.bus — Low-level bus client.
#
# Thin re-export of the historical `adapters/bus_client.BusClient`
# so the new SDK package and the old adapter scripts can coexist.
#
# New code should prefer `hydragent_py.HydraClient` (the high-level
# wrapper) over instantiating `BusClient` directly. This module is
# kept for:
#
#   • backwards compatibility with channel adapters that already
#     depend on `bus_client.BusClient`
#   • tests and tools that need fine-grained control of the wire
#     protocol (e.g. injecting faults, recording frames)

from .bus_impl import BusClient

__all__ = ["BusClient"]
