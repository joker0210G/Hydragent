"""Wait for the release binary to be produced (cargo build --release)."""
import os
import time
import sys
import json

p = os.path.join(os.path.dirname(__file__), "..", "target", "release", "hydragent.exe")
p = os.path.abspath(p)
deadline = time.time() + 600
log_path = os.path.join(os.path.dirname(__file__), "phase4_build_wait.log")

with open(log_path, "w", encoding="utf-8") as log:
    log.write(f"Watching {p}\n")
    while not os.path.exists(p):
        if time.time() > deadline:
            log.write(f"TIMEOUT at {time.time()}\n")
            print(json.dumps({"status": "TIMEOUT"}))
            sys.exit(1)
        time.sleep(5)
    sz = os.path.getsize(p)
    mtime = os.path.getmtime(p)
    log.write(f"OK at {time.time()}: size={sz} mtime={mtime}\n")
    print(json.dumps({"status": "OK", "size": sz, "mtime": mtime}))
