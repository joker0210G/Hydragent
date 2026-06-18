# hydragent_py.repl — Rich-based interactive REPL.
#
# Refactored from `adapters/cli_adapter.py` to be a reusable class
# (the old version was a one-shot script). The script `cli_adapter.py`
# now imports `REPL` from here so the entry point behaviour is
# unchanged for existing users.
#
# The REPL uses Rich for both input (Prompt.ask) and output (Console,
# Markdown). The kernel is reached over the JSON-RPC bus via
# `hydragent_py.client.HydraClient`.

from __future__ import annotations

import argparse
import re
import sys
from typing import Optional

from rich.console import Console
from rich.markdown import Markdown
from rich.prompt import Prompt
from rich.text import Text

from .client import HydraClient, HydraConfig, HydraError


console = Console()


# Regexes used to classify the free-form status strings the kernel
# emits via `response.status`. We translate them into quiet, user-
# friendly annotations instead of dumping raw markdown at the user.
_RE_STRATEGY = re.compile(
    r"`?\[Strategy:\s*(?P<name>[^`\]]+?)\s*(?:[—-]+\s*via\s*(?P<src>[^`\]]+))?\]`?"
)
_RE_THINKING = re.compile(r"`?\[Thinking\s*\(Step\s*(?P<step>\d+)/(?P<max>\d+)\)\][^`\]]*`?")
_RE_THOUGHT = re.compile(r"`?\[Thought\]`?\s*(?P<text>.*)")
_RE_TOOL = re.compile(
    r"`?\[Calling tool\]`?\s*\*\*(?P<name>[^*]+)\*\*\s*with\s*params\s*`(?P<params>[^`]*)`"
)
_RE_INJECT = re.compile(r"`?\[Injected\s+(?P<n>\d+)\s+facts[^\]]*\]`?")
_RE_PENDING = re.compile(r"`?\[Pending clarification:\s*\"(?P<q>[^\"]+)\"[^]]*\]`?")
_RE_DISCARD = re.compile(r"`?\[Discarded pending clarification:\s*\"(?P<q>[^\"]+)\"[^]]*\]`?")


def _format_status(status: str) -> Optional[Text]:
    """Translate a raw kernel status string into a polished Rich line.

    Returns ``None`` for statuses we want to hide entirely (e.g. the
    verbose "[Thought]" block, or a repeated "[Thinking]" step that
    will be replaced by the next one via :class:`StatusSpinner`).
    """
    s = status.strip()
    if not s:
        return None

    m = _RE_STRATEGY.match(s)
    if m:
        name = m.group("name").strip()
        src = m.group("src")
        label = f"◆ strategy: {name}"
        if src:
            label += f"  ({src.strip()})"
        return Text(label, style="dim cyan")

    m = _RE_THINKING.match(s)
    if m:
        # Returned separately — StatusSpinner will update the running
        # line, so we just signal "thinking" with the step number.
        return Text(f"⠋ thinking… step {m.group('step')}/{m.group('max')}", style="dim")

    m = _RE_THOUGHT.match(s)
    if m:
        # Hidden by default — the LLM's internal monologue is noisy.
        return None

    m = _RE_TOOL.match(s)
    if m:
        return Text(f"→ {m.group('name').strip()}({m.group('params')})", style="dim yellow")

    m = _RE_INJECT.match(s)
    if m:
        n = m.group("n")
        suffix = "" if n == "1" else "s"
        return Text(f"◆ recalled {n} fact{suffix} from memory", style="dim cyan")

    m = _RE_PENDING.match(s)
    if m:
        return Text(f"? pending clarification: {m.group('q')}", style="dim yellow")

    m = _RE_DISCARD.match(s)
    if m:
        return Text(f"◆ dropped pending clarification: {m.group('q')}", style="dim")

    # Unknown status — show it dimmed so the user still sees it but
    # doesn't mistake it for a real assistant message.
    return Text(s, style="dim italic")


class StatusSpinner:
    """Single-line "thinking…" indicator that updates in place.

    Used while we wait for the first real token from the kernel. The
    label is updated on each ``[Thinking (Step N/M)]`` status. When
    the assistant actually starts streaming tokens, the indicator is
    cleared via :meth:`clear` and never shown again.

    The spinner only writes a row if at least one :meth:`update` call
    has been made. That way, a kernel that skips the ``[Thinking]``
    phase entirely doesn't leave a dangling empty spinner line.
    """

    def __init__(self, console: Console) -> None:
        self.console = console
        self.label: str = "thinking…"
        self._shown = False
        self._cleared = False

    def update(self, label: str) -> None:
        if self._cleared:
            return
        self.label = label
        self.console.print(f"\r\x1b[2K  [dim]{self.label}[/dim]", end="")
        self._shown = True

    def clear(self) -> None:
        if self._cleared or not self._shown:
            self._cleared = True
            return
        self._cleared = True
        # Clear the line so the streamed tokens start on a clean row.
        self.console.print("\r\x1b[2K", end="")


class REPL:
    """Rich-based interactive REPL on top of the Hydragent kernel.

    >>> REPL(HydraConfig.from_env()).run()

    The REPL accepts slash commands inherited from the Rust kernel
    (e.g. `/exit`, `/help`) and forwards everything else as a regular
    user message. Permission prompts are routed through a Rich
    yes/no dialog.
    """

    def __init__(self, config: Optional[HydraConfig] = None):
        self.config = config or HydraConfig.from_env()
        self.console = Console()
        self.client: Optional[HydraClient] = None

    def run(self) -> int:
        """Run the REPL. Returns the process exit code."""
        try:
            self.client = HydraClient.connect(self.config)
        except HydraError as exc:
            self.console.print(f"[bold red]✗ Connection failed:[/bold red] {exc}")
            self.console.print(
                "[yellow]Please ensure the Rust core is running with "
                "`hydragent serve` (or `cargo run --bin hydragent` from source).[/yellow]"
            )
            return 1

        self._print_banner()
        return self._main_loop()

    def _print_banner(self) -> None:
        self.console.print()
        self.console.print("[bold cyan]🐉 Hydragent[/bold cyan] — Local AI Agent")
        self.console.print(
            f"Page ID: [dim]{self.config.page_id}[/dim]   "
            f"(type [bold red]exit[/bold red] to quit)"
        )
        self.console.print(f"[green]✓ Connected to {self.config.bus_host}:{self.config.bus_port}[/green]")
        self.console.print()

    def _main_loop(self) -> int:
        while True:
            try:
                user_input = Prompt.ask("[cyan]You ›[/cyan]")
            except (EOFError, KeyboardInterrupt):
                self.console.print("\n[dim]Goodbye.[/dim]")
                return 0

            if not user_input.strip():
                continue

            if user_input.strip().lower() in ("exit", "quit"):
                self.console.print("[dim]Goodbye.[/dim]")
                return 0

            try:
                self._handle_turn(user_input)
            except HydraError as exc:
                self.console.print(f"\n[bold red]✗ {exc}[/bold red]\n")
            except KeyboardInterrupt:
                self.console.print("\n[dim](interrupted)[/dim]")
            except Exception as exc:  # noqa: BLE001
                self.console.print(f"\n[bold red]✗ Transaction error:[/bold red] {exc}\n")

    def _handle_turn(self, user_input: str) -> None:
        assert self.client is not None
        self.console.print("[bold green]hydra ▸[/bold green]")

        spinner = StatusSpinner(self.console)
        streamed_chars = 0
        streamed: list[str] = []

        def on_token(token: str) -> None:
            nonlocal streamed_chars
            # Once we start streaming tokens, the spinner is done.
            spinner.clear()

            # The kernel sometimes prepends a stray newline to the
            # first streamed chunk (see react_loop.rs:326). Strip a
            # single leading newline only on the very first token to
            # keep the assistant row flush with the label.
            t = token
            if streamed_chars == 0 and t.startswith("\n"):
                t = t.lstrip("\n")

            # Stream the token straight to the terminal. We don't
            # re-render Markdown — the final answer is rendered
            # separately below only if the kernel returned no tokens
            # (rare; the kernel usually streams).
            self.console.print(t, end="", highlight=False)
            streamed.append(t)
            streamed_chars += len(t)

        def on_status(status: str) -> None:
            # If we're already streaming tokens, late-arriving status
            # frames are useless to the user — drop them.
            if streamed_chars > 0:
                return
            line = _format_status(status)
            if line is None:
                return
            # The "thinking… step N/M" statuses update a single
            # in-place line; everything else prints on its own row.
            if line.plain.startswith("⠋ thinking"):
                spinner.update(line.plain)
            else:
                # A non-thinking status arrived — close out the
                # spinner if it was showing, then print this line.
                spinner.clear()
                self.console.print(line)

        def on_permission(params: dict) -> bool:
            spinner.clear()
            self.console.print(f"\n[bold yellow][!] Approval Required[/bold yellow]")
            self.console.print(f"Tool: [cyan]{params['tool_id']}[/cyan]")
            self.console.print(f"Summary: [dim]{params['params_summary']}[/dim]")
            decision = Prompt.ask("Allow action?", choices=["y", "n"], default="n")
            approved = decision.lower() == "y"
            if approved:
                self.console.print("[green]✓ Action Approved.[/green]")
            else:
                self.console.print("[red]✗ Action Denied.[/red]")
            return approved

        final = self.client.chat(
            user_input,
            on_token=on_token,
            on_status=on_status,
            on_permission=on_permission,
        )

        spinner.clear()

        if streamed_chars == 0:
            # Kernel didn't stream anything (e.g. error path or
            # immediate refusal). Render the final answer as Markdown
            # so the user still gets a nicely formatted response.
            body = final or "*(no response)*"
            self.console.print(Markdown(body))
        else:
            # We already streamed the assistant's reply in real time.
            # Just close out the row with a trailing newline.
            self.console.print()
        self.console.print()


def run_repl() -> int:
    """Console-script entry point for `hydra-cli chat`."""
    parser = argparse.ArgumentParser(description="Hydragent interactive REPL")
    parser.add_argument(
        "--page", type=str, help="Specific Page ID to join or resume (legacy alias: --session)"
    )
    parser.add_argument("--session", type=str, help=argparse.SUPPRESS)  # legacy alias
    args = parser.parse_args()

    config = HydraConfig.from_env()
    if args.page or args.session:
        config.page_id = args.page or args.session
    return REPL(config).run()


if __name__ == "__main__":  # pragma: no cover
    sys.exit(run_repl())
