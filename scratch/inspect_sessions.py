import sqlite3
import os

db_path = "data/sessions.db"
if os.path.exists(db_path):
    conn = sqlite3.connect(db_path)
    cursor = conn.cursor()
    
    cursor.execute("SELECT name FROM sqlite_master WHERE type='table'")
    tables = [r[0] for r in cursor.fetchall()]
    
    for table in ["nodes", "edges", "session_meta", "messages", "tool_calls"]:
        if table in tables:
            cursor.execute(f"SELECT COUNT(*) FROM {table}")
            count = cursor.fetchone()[0]
            print(f"Table '{table}': {count} rows")
            
            if table == "session_meta":
                cursor.execute("SELECT session_id, last_active FROM session_meta")
                for r in cursor.fetchall():
                    print(f"  - Session ID: {r[0]}, Last Active: {r[1]}")
    conn.close()
