import os, glob
logs = sorted(glob.glob(r"f:\Workspace(temp)\repo\ai agent\scratch\bench_test*.log"),
              key=os.path.getmtime, reverse=True)
if not logs:
    print("NO LOGS")
else:
    log = logs[0]
    print("FILE:", log, "SIZE:", os.path.getsize(log))
    with open(log, 'rb') as f:
        data = f.read()
    if data[:2] == b'\xff\xfe':
        text = data.decode('utf-16-le')
    else:
        text = data.decode('utf-8', errors='replace')
    print(text[-3000:])
