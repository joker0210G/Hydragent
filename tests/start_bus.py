"""Start the hydragent bus in background and verify it's listening on 5000."""
import os
import subprocess
import time
import sys

repo = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
bin_path = os.path.join(repo, "target", "release", "hydragent.exe")
logs_dir = os.path.join(repo, "logs")
os.makedirs(logs_dir, exist_ok=True)
log_path = os.path.join(logs_dir, "hydragent_bus.log")
pid_path = os.path.join(logs_dir, "hydragent_bus.pid")

# Check if already running
if os.path.exists(pid_path):
    try:
        with open(pid_path) as f:
            old_pid = int(f.read().strip())
        # Check if process still alive
        import ctypes
        PROCESS_QUERY_LIMITED = 0x1000
        h = ctypes.windll.kernel32.OpenProcess(PROCESS_QUERY_LIMITED, False, old_pid)
        if h:
            ctypes.windll.kernel32.CloseHandle(h)
            print(f"hydragent already running PID={old_pid}")
            sys.exit(0)
    except Exception:
        pass

# Start the bus
with open(log_path, "wb") as logf:
    proc = subprocess.Popen(
        [bin_path],
        cwd=repo,
        stdout=logf,
        stderr=subprocess.STDOUT,
        creationflags=subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS,
    )
with open(pid_path, "w") as f:
    f.write(str(proc.pid))

# Wait for it to bind 5000
import socket
ok = False
for i in range(30):
    try:
        with socket.create_connection(("127.0.0.1", 5000), timeout=0.5):
            ok = True
            break
    except Exception:
        time.sleep(1)

if ok:
    print(f"hydragent started PID={proc.pid}, port 5000 is open")
    sys.exit(0)
else:
    print(f"hydragent failed to bind 5000 within 30s. Log:")
    try:
        with open(log_path, "rb") as f:
            print(f.read().decode("utf-8", "replace")[-2000:])
    except Exception:
        pass
    sys.exit(1)
