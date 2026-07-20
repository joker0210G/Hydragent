use async_trait::async_trait;
use hydragent_types::{ToolResult, ToolStatus, PermissionTier};
use crate::tool_trait::Tool;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

/// Execute short scripts in a temporary scratch directory and capture
/// stdout / stderr / exit code. Supports Python and shell.
///
/// # Sandbox reality (read this before adding more "isolation")
///
/// This is a **subprocess sandbox**, not a kernel-isolated one. What
/// it actually provides today (stdlib only, no extra deps):
///
///   * **Hard wall-clock timeout** via `tokio::time::timeout` plus
///     `Child::kill()` if the deadline fires. Hard ceiling is 60s.
///   * **Per-invocation scratch dir** under `<workspace>/cache/code_execute/<uuid>/`,
///     used as the child's CWD. Removed best-effort on exit.
///   * **Captured & capped stdout/stderr** (16 KiB each) so a runaway
///     script can't blow up the agent's memory. Pipe is drained past
///     the cap (discarded) so the child never blocks on a full pipe.
///   * **Clean environment** via `env_clear()` + a tiny whitelist
///     (`PATH`, `PYTHONIOENCODING`, `PYTHONDONTWRITEBYTECODE`,
///     `PYTHONNOUSERSITE`, `NO_PROXY=*`, `SYSTEMROOT`).
///     Parent secrets (vault passphrase, `*_API_KEY`, …) do NOT leak
///     into the child.
///   * **Python `-I`** ("isolated mode"): skips user site-packages,
///     suppresses `.pyc` writes, runs without `site.py`.
///   * **`stdin` closed** so the script cannot block on input.
///
/// What this implementation does **NOT** provide (be honest):
///
///   * ❌ **No memory cap.** A child that allocates gigabytes will
///     keep allocating until the wall-clock timeout fires.
///     (Proper fix: `rlimit` via `libc`/`nix` on Unix, `JobObject`
///     via `windows-sys` on Windows — both avoided here per
///     ponytail-minimalism: no new dependencies.)
///   * ❌ **No network isolation.** The script can `import urllib`
///     or `curl https://…` and reach the open internet.
///     (Proper fix: OS-level firewall rules / network namespaces —
///     not stdlib.)
///   * ❌ **No filesystem chroot.** Absolute paths (`/etc/passwd`,
///     `C:\Windows\…`) remain reachable. The CWD sandbox only blocks
///     accidental relative access.
///     (Proper fix: Linux namespaces / macOS sandbox-exec /
///     Windows Job Object + restricted token — not stdlib.)
///
/// Until those three are wired up, treat `code_execute` as **trusted-
/// code execution**, not as an untrusted-code sandbox. Permission tier
/// is `Prompt` so a human approves every call.
pub struct CodeExecuteTool {
    workspace_dir: PathBuf,
    default_timeout_ms: u64,
    max_output_bytes: usize,
}

impl CodeExecuteTool {
    pub fn new(workspace_dir: impl Into<PathBuf>) -> Self {
        Self {
            workspace_dir: workspace_dir.into(),
            default_timeout_ms: 15_000,
            max_output_bytes: 16 * 1024,
        }
    }
}

#[async_trait]
impl Tool for CodeExecuteTool {
    fn name(&self) -> &str {
        "code_execute"
    }

    fn description(&self) -> &str {
        "Run a short Python or shell script in an isolated scratch directory and return its \
         stdout, stderr, exit code, and execution time. Hard wall-clock timeout (default 15s, max 60s). \
         Use for arithmetic, data wrangling, file inspection, or any computation the model can't do \
         in its head. NOT a security boundary — see tool docs for what is and isn't isolated."
    }

    fn permission_tier(&self) -> PermissionTier {
        // Running user-supplied code is high-impact. Require human approval.
        PermissionTier::Prompt
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "language": {
                    "type": "string",
                    "enum": ["python", "bash", "sh"],
                    "description": "Interpreter family. 'python' (default) and 'bash'/'sh' are supported. 'bash' falls back to 'sh' on Unix and 'cmd.exe' on Windows."
                },
                "code": {
                    "type": "string",
                    "description": "Source code to execute. Interpreted by the chosen language."
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 100,
                    "maximum": 60000,
                    "description": "Hard wall-clock timeout in milliseconds. Defaults to 15000 (15s). Capped at 60000 (60s)."
                }
            },
            "required": ["code"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = Instant::now();

        let val: Value = match serde_json::from_str(params_json) {
            Ok(v) => v,
            Err(e) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: "{}".into(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("Invalid parameters: {}", e)),
                };
            }
        };

        let code = match val.get("code").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            Some(_) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: "{}".into(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some("'code' parameter is empty".into()),
                };
            }
            None => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: "{}".into(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some("Missing required parameter: 'code'".into()),
                };
            }
        };

        let language = val
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("python")
            .to_ascii_lowercase();

        // Cap timeout: 100ms..=60s. Out-of-range values are clamped silently
        // rather than rejected — the LLM occasionally picks absurd numbers
        // and we'd rather run a tighter cap than fail outright.
        let timeout_ms = val
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.default_timeout_ms)
            .clamp(100, 60_000);

        // Scratch dir lives under the workspace cache. UUID prevents any
        // collision between concurrent invocations.
        let scratch_dir = self
            .workspace_dir
            .join("cache")
            .join("code_execute")
            .join(Uuid::new_v4().to_string());

        if let Err(e) = tokio::fs::create_dir_all(&scratch_dir).await {
            return ToolResult {
                call_id: String::new(),
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to create scratch dir: {}", e)),
            };
        }

        let result = match language.as_str() {
            "python" | "py" => self.run_python(&scratch_dir, code, timeout_ms, start).await,
            // ponytail: "bash" is a Unix-only interpreter. On Windows we map
            // to cmd.exe so the tool still works in dev. The semantics
            // differ; the doc string warns the LLM about this.
            "bash" | "sh" => self.run_shell(&scratch_dir, code, timeout_ms, start).await,
            other => ToolResult {
                call_id: String::new(),
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!(
                    "Unsupported language '{}'. Supported: 'python', 'bash', 'sh'.",
                    other
                )),
            },
        };

        // Best-effort cleanup. If this fails (file locked, process still
        // holding a handle), the scratch dir leaks; periodic janitor
        // can prune old dirs under cache/code_execute/.
        let _ = tokio::fs::remove_dir_all(&scratch_dir).await;

        result
    }
}

impl CodeExecuteTool {
    async fn run_python(
        &self,
        scratch_dir: &PathBuf,
        code: &str,
        timeout_ms: u64,
        start: Instant,
    ) -> ToolResult {
        let script_path = scratch_dir.join("script.py");
        if let Err(e) = tokio::fs::write(&script_path, code).await {
            return ToolResult {
                call_id: String::new(),
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to write script: {}", e)),
            };
        }

        // Probe for a working interpreter. If none is on PATH, return a
        // clean Failure instead of a confusing spawn error from the OS.
        let interpreter = match probe_interpreter(&["python3", "python", "py"]) {
            Some(p) => p,
            None => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: "{}".into(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(
                        "No Python interpreter found on PATH (tried: python3, python, py)"
                            .into(),
                    ),
                };
            }
        };

        let mut cmd = Command::new(&interpreter);
        // -I: isolated mode (Python 3.11+). Equivalent to -S -E -s + the
        // PYTHON* env vars below, but a single flag is more robust than
        // trying to remember each individual one.
        cmd.arg("-I")
            .arg(&script_path)
            .current_dir(scratch_dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PYTHONNOUSERSITE", "1")
            .env("SYSTEMROOT", std::env::var("SYSTEMROOT").unwrap_or_default())
            // Discouragement-only. Does NOT actually firewall the process;
            // see struct docs.
            .env("NO_PROXY", "*")
            .env("no_proxy", "*")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        run_with_timeout(cmd, timeout_ms, start, self.max_output_bytes).await
    }

    async fn run_shell(
        &self,
        scratch_dir: &PathBuf,
        code: &str,
        timeout_ms: u64,
        start: Instant,
    ) -> ToolResult {
        let (interpreter, flag) = shell_command();
        let script_path = scratch_dir.join(script_filename());

        if let Err(e) = tokio::fs::write(&script_path, code).await {
            return ToolResult {
                call_id: String::new(),
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to write script: {}", e)),
            };
        }

        let mut cmd = Command::new(interpreter);
        cmd.arg(flag)
            .arg(&script_path)
            .current_dir(scratch_dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("SYSTEMROOT", std::env::var("SYSTEMROOT").unwrap_or_default())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        run_with_timeout(cmd, timeout_ms, start, self.max_output_bytes).await
    }
}

/// Spawn the command, wait for it to exit OR hit the timeout, and
/// collect stdout/stderr with a per-stream byte cap. On timeout the
/// child is killed so its stdout/stderr handles close and the
/// drain tasks finish.
async fn run_with_timeout(
    mut cmd: Command,
    timeout_ms: u64,
    start: Instant,
    max_output_bytes: usize,
) -> ToolResult {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ToolResult {
                call_id: String::new(),
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to spawn process: {}", e)),
            };
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Two independent drains, each capped at `max_output_bytes`.
    // Past the cap, we keep reading-and-discarding so the writer side
    // of the pipe never blocks on a full kernel buffer.
    let stdout_task = tokio::spawn(async move {
        match stdout {
            Some(s) => drain_capped(s, max_output_bytes).await,
            None => (Vec::new(), false),
        }
    });
    let stderr_task = tokio::spawn(async move {
        match stderr {
            Some(s) => drain_capped(s, max_output_bytes).await,
            None => (Vec::new(), false),
        }
    });

    let deadline = Duration::from_millis(timeout_ms);
    let wait_result = timeout(deadline, child.wait()).await;

    match wait_result {
        // Process exited within the deadline.
        Ok(Ok(status)) => {
            // Collect the drain tasks. If either panicked (it shouldn't),
            // we fall back to empty buffers.
            let stdout_bytes = stdout_task
                .await
                .ok()
                .map(|(b, _)| b)
                .unwrap_or_default();
            let stderr_bytes = stderr_task
                .await
                .ok()
                .map(|(b, _)| b)
                .unwrap_or_default();
            let stdout_truncated = stdout_bytes.len() >= max_output_bytes;
            let stderr_truncated = stderr_bytes.len() >= max_output_bytes;

            let stdout_str = String::from_utf8_lossy(&stdout_bytes).into_owned();
            let stderr_str = String::from_utf8_lossy(&stderr_bytes).into_owned();

            let payload = json!({
                "stdout": stdout_str,
                "stderr": stderr_str,
                "exit_code": status.code(),
                "timed_out": false,
                "truncated": stdout_truncated || stderr_truncated,
                "max_output_bytes": max_output_bytes,
                "duration_ms": start.elapsed().as_millis() as u64,
                "timeout_ms": timeout_ms,
            });

            ToolResult {
                call_id: String::new(),
                output_json: serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into()),
                status: if status.success() {
                    ToolStatus::Success
                } else {
                    ToolStatus::Failure
                },
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: if status.success() {
                    None
                } else {
                    Some(format!(
                        "Process exited with code {}",
                        status.code().map(|c| c.to_string()).unwrap_or_else(|| "<signal>".into())
                    ))
                },
            }
        }
        // Process spawned, then the wait itself failed (rare — usually an
        // I/O error talking to the child handle).
        Ok(Err(e)) => {
            // Try to reap the child if it's still around.
            let _ = child.kill().await;
            let _ = child.wait().await;
            ToolResult {
                call_id: String::new(),
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("Process wait failed: {}", e)),
            }
        }
        // Wall-clock deadline elapsed. Kill the child and report Timeout.
        Err(_elapsed) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            // Reap whatever the drains had buffered. After kill() the
            // pipes will close and these complete promptly.
            let _ = stdout_task.await;
            let _ = stderr_task.await;

            let payload = json!({
                "stdout": "",
                "stderr": "",
                "exit_code": null,
                "timed_out": true,
                "truncated": false,
                "max_output_bytes": max_output_bytes,
                "duration_ms": start.elapsed().as_millis() as u64,
                "timeout_ms": timeout_ms,
            });
            ToolResult {
                call_id: String::new(),
                output_json: serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into()),
                status: ToolStatus::Timeout,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!(
                    "Execution exceeded the {}ms hard timeout and was killed.",
                    timeout_ms
                )),
            }
        }
    }
}

/// Read up to `cap` bytes into `out`, then drain-and-discard the rest
/// so the writer doesn't block. Returns `(bytes_kept, was_truncated)`.
async fn drain_capped<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    cap: usize,
) -> (Vec<u8>, bool) {
    let mut kept: Vec<u8> = Vec::with_capacity(4096.min(cap.max(1)));
    let mut chunk = vec![0u8; 4096];

    loop {
        if kept.len() >= cap {
            // Drain remaining bytes so the writer's pipe never fills.
            // We don't care about content past the cap.
            let mut sink = [0u8; 4096];
            loop {
                match reader.read(&mut sink).await {
                    Ok(0) | Err(_) => return (kept, true),
                    Ok(_) => continue,
                }
            }
        }
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                let take = n.min(room);
                kept.extend_from_slice(&chunk[..take]);
            }
            Err(_) => break,
        }
    }
    let truncated = kept.len() >= cap;
    (kept, truncated)
}

/// Try each candidate interpreter name and return the first one whose
/// `--version` probe succeeds. We probe rather than just spawning the
/// first name so the error message is actionable ("python not found")
/// instead of opaque ("exit code 9009").
fn probe_interpreter(candidates: &[&str]) -> Option<String> {
    use std::process::Command as StdCommand;
    for c in candidates {
        if StdCommand::new(c)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some((*c).to_string());
        }
    }
    None
}

#[cfg(unix)]
fn shell_command() -> (&'static str, &'static str) {
    // -c reads the script from argv; lets us pass it as a file path
    // rather than a string (avoids quoting issues for code containing
    // quotes, newlines, etc.).
    ("sh", "-c")
}

#[cfg(windows)]
fn shell_command() -> (&'static str, &'static str) {
    // /c reads the script as a file path. cmd.exe has different
    // semantics from bash; the tool docs note this.
    ("cmd.exe", "/c")
}

#[cfg(unix)]
fn script_filename() -> &'static str {
    "script.sh"
}

#[cfg(windows)]
fn script_filename() -> &'static str {
    "script.bat"
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────
//
// All tests use Python if available; they short-circuit (return
// without asserting) when no interpreter is on PATH so the suite still
// passes on minimal CI images. The success test does a real `print`;
// the error test exits non-zero; the timeout test sleeps past its
// own deadline. The syntax-error test covers the most common
// non-runtime failure mode.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// Memoised interpreter probe so each test doesn't re-spawn `python --version`.
    fn python_interpreter() -> Option<&'static str> {
        static FOUND: OnceLock<Option<String>> = OnceLock::new();
        FOUND
            .get_or_init(|| probe_interpreter(&["python3", "python", "py"]))
            .as_deref()
    }

    fn skip_if_no_python(test_name: &str) -> Option<&'static str> {
        match python_interpreter() {
            Some(p) => Some(p),
            None => {
                eprintln!("[{}] skipping — no python on PATH", test_name);
                None
            }
        }
    }

    fn make_tool() -> CodeExecuteTool {
        let workspace = std::env::temp_dir().join(format!(
            "hydra_code_exec_test_{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        CodeExecuteTool::new(workspace)
    }

    #[tokio::test]
    async fn test_code_execute_success() {
        let Some(py) = skip_if_no_python("test_code_execute_success") else {
            return;
        };
        let tool = make_tool();

        let res = tool
            .execute(&format!(
                r#"{{"language":"python","code":"print('hello from python ({py})')\n","timeout_ms":5000}}"#
            ))
            .await;

        assert_eq!(
            res.status,
            ToolStatus::Success,
            "expected Success, got {:?} ({:?})",
            res.status,
            res.error_message
        );
        assert!(res.error_message.is_none(), "no error_message on success");

        let payload: Value = serde_json::from_str(&res.output_json).unwrap();
        assert_eq!(payload["exit_code"], json!(0));
        assert_eq!(payload["timed_out"], json!(false));
        assert!(
            payload["stdout"]
                .as_str()
                .unwrap_or("")
                .contains("hello from python"),
            "stdout should contain the script's output: {}",
            payload["stdout"]
        );
        assert!(
            payload["duration_ms"].as_u64().unwrap() < 5_000,
            "should finish well under the 5s timeout"
        );
    }

    #[tokio::test]
    async fn test_code_execute_nonzero_exit_is_failure() {
        let Some(_py) = skip_if_no_python("test_code_execute_nonzero_exit_is_failure") else {
            return;
        };
        let tool = make_tool();

        let res = tool
            .execute(
                r#"{"language":"python","code":"import sys\nsys.exit(2)\n","timeout_ms":5000}"#,
            )
            .await;

        // The process ran to completion; the tool itself didn't fail.
        // But the underlying script exited 2, so we surface that as
        // ToolStatus::Failure with a structured exit_code.
        assert_eq!(res.status, ToolStatus::Failure);

        let payload: Value = serde_json::from_str(&res.output_json).unwrap();
        assert_eq!(payload["exit_code"], json!(2));
        assert_eq!(payload["timed_out"], json!(false));
        assert!(
            res.error_message
                .as_deref()
                .unwrap_or("")
                .contains("exited with code 2"),
            "error_message should mention exit code: {:?}",
            res.error_message
        );
    }

    #[tokio::test]
    async fn test_code_execute_timeout_kills_runaway() {
        let Some(_py) = skip_if_no_python("test_code_execute_timeout_kills_runaway") else {
            return;
        };
        let tool = make_tool();

        // Script tries to sleep 30s; we cap it at 500ms. Wall-clock
        // must hold the line: child is killed and we return Timeout.
        let res = tool
            .execute(
                r#"{"language":"python","code":"import time\ntime.sleep(30)\n","timeout_ms":500}"#,
            )
            .await;

        assert_eq!(
            res.status,
            ToolStatus::Timeout,
            "expected Timeout, got {:?} (err: {:?}, payload: {})",
            res.status,
            res.error_message,
            res.output_json
        );

        let payload: Value = serde_json::from_str(&res.output_json).unwrap();
        assert_eq!(payload["timed_out"], json!(true));
        assert_eq!(payload["timeout_ms"], json!(500));
        assert_eq!(payload["exit_code"], Value::Null);
        assert!(
            res.error_message
                .as_deref()
                .unwrap_or("")
                .contains("500ms"),
            "error_message should mention the timeout: {:?}",
            res.error_message
        );
        // And critically: we must not have actually waited the full 30s.
        assert!(
            payload["duration_ms"].as_u64().unwrap_or(u64::MAX) < 5_000,
            "we should kill the child long before the 30s sleep finishes"
        );
    }

    #[tokio::test]
    async fn test_code_execute_syntax_error() {
        let Some(_py) = skip_if_no_python("test_code_execute_syntax_error") else {
            return;
        };
        let tool = make_tool();

        let res = tool
            .execute(
                r#"{"language":"python","code":"this is :: not :: python\n","timeout_ms":5000}"#,
            )
            .await;

        // Python returns a non-zero exit code on SyntaxError, so the
        // tool maps that to Failure just like any other non-zero exit.
        assert_eq!(res.status, ToolStatus::Failure);

        let payload: Value = serde_json::from_str(&res.output_json).unwrap();
        // SyntaxError exits with code 1 on CPython.
        assert_eq!(payload["exit_code"], json!(1));
        assert!(
            payload["stderr"]
                .as_str()
                .unwrap_or("")
                .contains("SyntaxError"),
            "stderr should mention SyntaxError: {}",
            payload["stderr"]
        );
    }
}