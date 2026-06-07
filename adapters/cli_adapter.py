import asyncio
import sys
import uuid
import argparse
from datetime import datetime
from rich.console import Console
from rich.markdown import Markdown
from rich.prompt import Prompt
from bus_client import BusClient

console = Console()

async def main():
    parser = argparse.ArgumentParser(description="Hydragent CLI Chat Adapter")
    parser.add_argument("--session", type=str, help="Specific session ID to join or resume")
    args = parser.parse_args()

    session_id = args.session if args.session else str(uuid.uuid4())
    user_id = "local-user"
    channel_id = "cli:default"

    bus = BusClient()
    
    console.print("\n[bold cyan]🐉 Hydragent[/bold cyan] v0.1.0 — Local AI Agent")
    console.print(f"Session ID: [dim]{session_id}[/dim] (type [bold red]exit[/bold red] to quit)")
    console.print("Connecting to Event Bus...")
    
    try:
        await bus.connect()
        console.print("[green]✓ Connected successfully![/green]\n")
    except Exception as e:
        console.print(f"[bold red]✗ Connection failed:[/bold red] {e}")
        console.print("[yellow]Please ensure the Rust core is running with 'cargo run --bin hydragent'[/yellow]")
        sys.exit(1)

    while True:
        try:
            user_input = await asyncio.get_event_loop().run_in_executor(
                None, lambda: Prompt.ask("[cyan]You ›[/cyan]")
            )
        except (EOFError, KeyboardInterrupt):
            console.print("\n[dim]Goodbye.[/dim]")
            break

        if not user_input.strip():
            continue

        if user_input.strip().lower() in ("exit", "quit"):
            console.print("[dim]Goodbye.[/dim]")
            break

        import time
        event = {
            "session_id": session_id,
            "channel_id": channel_id,
            "user_id":    user_id,
            "content":    user_input,
            "attachments": [],
            "metadata":   {},
            "timestamp":  int(time.time() * 1000),
            "priority":   "normal",
        }


        console.print("[green]Hydra ›[/green] ", end="")
        
        # Buffer to keep track of tokens printed
        streamed_tokens = []

        def on_token(token):
            print(token, end="", flush=True)
            streamed_tokens.append(token)

        try:
            # send_intent sends the message and triggers on_token for stream tokens
            final_content = await bus.send_intent(event, token_callback=on_token)
            
            # Print a newline after streaming is finished
            print()
            
            # If the streamed tokens didn't output anything, print the final content
            if not streamed_tokens:
                console.print(Markdown(final_content))
                
            console.print()
        except Exception as e:
            console.print(f"\n[bold red]✗ Transaction error:[/bold red] {e}\n")

if __name__ == "__main__":
    asyncio.run(main())
