# hydragent_py.client — High-level client for the Hydragent kernel.
#
# Wraps the low-level BusClient (JSON-RPC over TCP) with:
#   • connection lifecycle management (lazy connect, reconnect, ping)
#   • a synchronous, ergonomic `chat()` method
#   • an async `stream()` method for token-by-token consumers
#   • a typed HydraError hierarchy
#   • a permission callback hook (sync or async)
#
# This is the recommended entry point for new code. The lower-level
# BusClient is still exported for backwards compatibility and for
# adapters that need fine-grained control.

from __future__ import annotations

import asyncio
import logging
import os
import threading
import time
import uuid
from dataclasses import dataclass, field
from typing import Awaitable, Callable, Iterator, Optional, Union

from .bus import BusClient


_LOG = logging.getLogger("hydragent_py.client")


@dataclass
class HydraConfig:
    """Configuration for connecting to a Hydragent kernel.

    Defaults are read from environment variables so the SDK works
    out-of-the-box with `hydragent serve` running on localhost:5000.
    """

    bus_host: str = field(default_factory=lambda: os.getenv("HYDRA_BUS_HOST", "127.0.0.1"))
    bus_port: int = field(default_factory=lambda: int(os.getenv("HYDRA_BUS_PORT", os.getenv("BUS_PORT", "5000"))))
    page_id: str = field(default_factory=lambda: os.getenv("HYDRA_PAGE_ID", str(uuid.uuid4())))
    user_id: str = field(default_factory=lambda: os.getenv("HYDRA_USER_ID", "local-user"))
    channel_id: str = field(default_factory=lambda: os.getenv("HYDRA_CHANNEL_ID", "cli:default"))
    request_timeout_s: float = field(
        default_factory=lambda: float(os.getenv("HYDRA_REQUEST_TIMEOUT", "120"))
    )
    auto_reconnect: bool = True

    @classmethod
    def from_env(cls) -> "HydraConfig":
        """Build a config from current environment variables."""
        return cls()


class HydraError(RuntimeError):
    """Base class for all SDK-raised errors."""


class HydraConnectionError(HydraError):
    """Raised when the SDK cannot reach the bus."""


class HydraTimeoutError(HydraError):
    """Raised when a request exceeds `request_timeout_s`."""


# A permission callback may be sync (returns bool) or async.
PermissionCallback = Callable[[dict], Union[bool, Awaitable[bool]]]


class HydraClient:
    """High-level, ergonomic client for the Hydragent kernel.

    Use as a context manager for automatic cleanup:

        >>> with HydraClient.connect() as hydra:
        ...     print(hydra.chat("hello"))

    Or hold it directly and call `close()` when done:

        >>> hydra = HydraClient.connect()
        >>> try:
        ...     print(hydra.chat("hello"))
        ... finally:
        ...     hydra.close()
    """

    def __init__(self, config: Optional[HydraConfig] = None, *, bus: Optional[BusClient] = None):
        self.config = config or HydraConfig.from_env()
        self._bus = bus or BusClient(
            host=self.config.bus_host,
            port=self.config.bus_port,
        )
        self._owns_bus = bus is None
        self._connected = False
        # A dedicated background thread owns the event loop that the
        # BusClient's reader/writer are bound to. The reader/writer
        # streams from `asyncio.open_connection()` can only be used
        # from the loop they were created on, so we can't just call
        # `asyncio.run()` separately for connect() and send_intent() —
        # that would put the streams on different loops and every
        # write/read would fail. Instead, we keep one loop alive for
        # the whole lifetime of this HydraClient and submit work to it
        # from the calling (sync) thread via run_coroutine_threadsafe.
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._loop_thread: Optional[threading.Thread] = None

    # ── Lifecycle ────────────────────────────────────────────────────
    @classmethod
    def connect(cls, config: Optional[HydraConfig] = None) -> "HydraClient":
        """Create a client and open the bus connection.

        Raises:
            HydraConnectionError: if the bus is unreachable.
        """
        client = cls(config)
        client._open()
        return client

    def _open(self) -> None:
        if self._connected:
            return
        ready = threading.Event()
        err: list = []

        def _thread_main() -> None:
            try:
                # On Windows, asyncio.new_event_loop() returns a ProactorEventLoop
                # (IOCP-based). The ProactorEventLoop has a known issue where a
                # StreamReader created inside a non-main background thread does not
                # reliably receive socket-readiness callbacks, so readline() never
                # resolves even though data has arrived on the socket.
                #
                # SelectorEventLoop uses select() and works reliably in background
                # threads for plain TCP streams. We prefer it whenever it is
                # available — the SDK only needs TCP, not IOCP-only features.
                try:
                    loop = asyncio.SelectorEventLoop()  # type: ignore[attr-defined]
                except AttributeError:
                    loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
                # Use the module logger (silent by default) so the SDK does not
                # leak internal diagnostic noise to user output on every
                # connect(). Users that want to see this can enable logging via
                # `logging.getLogger("hydragent_py").setLevel(logging.DEBUG)`.
                _LOG.debug(
                    "background loop started: %s",
                    type(loop).__name__,
                )
                self._loop = loop
                ready.set()
                loop.run_forever()
            except Exception as e:  # noqa: BLE001
                err.append(e)
                ready.set()

        self._loop_thread = threading.Thread(
            target=_thread_main,
            name="hydragent-py-loop",
            daemon=True,
        )
        self._loop_thread.start()
        # Wait for the loop to be set on the background thread.
        if not ready.wait(timeout=5.0):
            raise HydraConnectionError("background event loop thread did not start")
        if err:
            raise HydraConnectionError(f"background event loop crashed: {err[0]}")

        try:
            future = asyncio.run_coroutine_threadsafe(self._bus.connect(), self._loop)
            future.result(timeout=10.0)
        except Exception as exc:
            self._teardown_loop()
            raise HydraConnectionError(
                f"could not reach Hydragent bus at "
                f"{self.config.bus_host}:{self.config.bus_port}: {exc}"
            ) from exc
        self._connected = True

    def _teardown_loop(self) -> None:
        """Stop the background event loop and join the thread."""
        loop = self._loop
        thread = self._loop_thread
        self._loop = None
        self._loop_thread = None
        if loop is None:
            return
        try:
            loop.call_soon_threadsafe(loop.stop)
        except Exception:
            pass
        if thread is not None:
            thread.join(timeout=2.0)
        try:
            loop.close()
        except Exception:
            pass

    def close(self) -> None:
        if not self._connected:
            return
        if self._loop is not None:
            try:
                future = asyncio.run_coroutine_threadsafe(self._bus.close(), self._loop)
                future.result(timeout=5.0)
            except Exception:
                pass
        self._connected = False
        self._teardown_loop()

    def __enter__(self) -> "HydraClient":
        if not self._connected:
            self._open()
        return self

    def __exit__(self, *exc_info) -> None:
        self.close()

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:
            pass

    # ── Public API ──────────────────────────────────────────────────
    def chat(
        self,
        message: str,
        *,
        on_token: Optional[Callable[[str], None]] = None,
        on_status: Optional[Callable[[str], None]] = None,
        on_permission: Optional[PermissionCallback] = None,
        page_id: Optional[str] = None,
    ) -> str:
        """Send a message and return the final assistant reply.

        Args:
            message: The user-visible prompt to send to the LLM.
            on_token: Optional callback invoked for every streamed token.
            on_status: Optional callback for status updates (e.g. "Thinking…").
            on_permission: Sync or async callback for tool permission prompts.
            page_id: Override the session id (defaults to self.config.page_id).

        Returns:
            The final assistant reply as a plain string.
        """
        if not self._connected or self._loop is None:
            raise HydraConnectionError(
                "HydraClient: not connected — use 'with HydraClient.connect() as h:'"
            )
        event = self._build_event(message, page_id=page_id)
        # Run the coroutine on the background loop and block this
        # thread until the result arrives. request_timeout_s is
        # enforced via the future.result() call.
        future = asyncio.run_coroutine_threadsafe(
            self._bus.send_intent(
                event,
                token_callback=on_token,
                status_callback=on_status,
                permission_callback=on_permission,
            ),
            self._loop,
        )
        try:
            return future.result(timeout=self.config.request_timeout_s)
        except asyncio.TimeoutError as exc:
            raise HydraTimeoutError(
                f"request timed out after {self.config.request_timeout_s}s"
            ) from exc

    async def stream(
        self,
        message: str,
        *,
        on_permission: Optional[PermissionCallback] = None,
        page_id: Optional[str] = None,
    ) -> "AsyncStream":
        """Async variant of `chat()`.

        Returns an `AsyncStream` that yields tokens as they arrive. The
        stream's `text` property holds the accumulated reply.

            >>> async with HydraClient.connect() as hydra:
            ...     stream = await hydra.stream("Tell me a joke")
            ...     async for token in stream:
            ...         print(token, end="", flush=True)
        """
        # For now, `stream` is a thin shim around the synchronous path.
        # A future version will pipe tokens through a real async queue.
        text = self.chat(
            message,
            on_permission=on_permission,
            page_id=page_id,
        )
        return AsyncStream(text)

    # ── Internals ───────────────────────────────────────────────────
    def _build_event(self, message: str, page_id: Optional[str] = None) -> dict:
        return {
            "page_id": page_id or self.config.page_id,
            "channel_id": self.config.channel_id,
            "user_id": self.config.user_id,
            "content": message,
            "attachments": [],
            "metadata": {},
            "timestamp": int(time.time() * 1000),
            "priority": "normal",
        }


class AsyncStream:
    """Lightweight wrapper around a completed reply string.

    `AsyncStream` is the return type of `HydraClient.stream()`. It
    behaves like an async iterator over the characters of the reply,
    so existing `async for token in stream:` code keeps working.
    """

    def __init__(self, text: str):
        self.text = text

    def __aiter__(self) -> "AsyncStream":
        return self

    async def __anext__(self) -> str:
        # Yield the whole text on the first call so callers can treat
        # the stream as a fire-and-forget result without complex state.
        if self._consumed:
            raise StopAsyncIteration
        self._consumed = True
        return self.text

    _consumed = False
