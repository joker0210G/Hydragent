import asyncio
import sys
from bus_client import BusClient

async def main():
    client = BusClient()
    print("Connecting to Hydragent Event Bus...")
    try:
        await client.connect()
        print("Connected! Sending test message...")
    except Exception as e:
        print(f"Failed to connect: {e}")
        print("Make sure you started the Rust core using 'cargo run --bin hydragent'!")
        sys.exit(1)

    event = {
        "page_id": "test-session",
        "channel_id": "cli:test",
        "user_id": "test-user",
        "content": "Hello Rust Core!",
        "attachments": [],
        "metadata": {},
        "timestamp": 1620000000000,
        "priority": "normal",
    }

    def on_token(token):
        print(token, end="", flush=True)

    print("Streamed output: ", end="")
    try:
        final_answer = await client.send_intent(event, token_callback=on_token)
        print("\nFinal response:", final_answer)
    except Exception as e:
        print(f"\nError during transaction: {e}")

if __name__ == "__main__":
    asyncio.run(main())
