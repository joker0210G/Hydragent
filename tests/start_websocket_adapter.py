"""Start the WebSocket adapter in background and verify it accepts connections."""
import os
import subprocess
import time
import sys
import socket

repo = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
adapter = os.path.join(repo, "adapters", "websocket_adapter.py")
python = os.path.join(repo, "adapters", ".venv", "Scripts", "python.exe")
logs_dir = os.path.join(repo, "logs")
os.makedirs(logs_dir, exist_ok=True)
log_path = os.path.join(logs_dir, "websocket_adapter.log")
pid_path = os.path.join(logs_dir, "websocket_adapter.pid")

if not os.path.exists(python):
    print(f"Python venv not found at {python}")
    sys.exit(1)

# Start adapter
with open(log_path, "wb") as logf:
    proc = subprocess.Popen(
        [python, "-u", adapter],
        cwd=repo,
        stdout=logf,
        stderr=subprocess.STDOUT,
        creationflags=subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS,
    )
with open(pid_path, "w") as f:
    f.write(str(proc.pid))

# Wait for it to bind 8765
port = int(os.getenv("WEBSOCKET_PORT", "8765"))
ok = False
for i in range(20):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.5):
            ok = True
            break
    except Exception:
        time.sleep(0.5)

if ok:
    print(f"websocket adapter started PID={proc.pid}, port {port} is open")
    sys.exit(0)
else:
    print(f"websocket adapter failed to bind {port} within 10s. Log:")
    try:
        with open(log_path, "rb") as f:
            print(f.read().decode("utf-8", "replace")[-2000:])
    except Exception:
        pass
    sys.exit(1)
