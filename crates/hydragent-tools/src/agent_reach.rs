// crates/hydragent-tools/src/agent_reach.rs
//
// agent_reach tool — bridges the hydragent LLM to the Agent-Reach ecosystem
// (https://github.com/Panniantong/Agent-Reach). Agent-Reach itself is an
// installer + doctor; this tool gives the LLM a single entry point to the
// channels that `agent-reach install --safe` already set up:
//
//   - jina_fetch  : any web page → Markdown (via Jina Reader, the universal channel)
//   - youtube     : YouTube URL  → metadata + optional subtitles (via yt-dlp)
//   - bilibili    : search query → top video hits (via B站 public search API)
//   - rss         : feed URL     → entries (via feedparser)
//   - github      : "owner/repo" → README (or any raw file via `path`/`branch`)
//   - doctor      : channel health report (delegates to `agent-reach doctor`)
//
// We shell out to `adapters/agent_reach_runner.py` (a stdlib-only Python
// wrapper that lives in the project venv) instead of inlining pyo3. Reasons:
//   1. The runner is a single, well-audited surface — we keep the heavy lifting
//      (yt-dlp, feedparser, Jina) in Python where the ecosystem is.
//   2. No extra Cargo build time / linker footprint (pyo3 pulls Python headers
//      and adds minutes to debug builds).
//   3. The subprocess boundary is a natural security moat: any future sandbox
//      work can constrain the spawned python.exe without touching Rust.
//
// ── JSON I/O contract (mirrored from agent_reach_runner.py) ────────────────
//   stdin  (text)  : JSON object {"command": "<channel>", ...}
//   stdout (text)  : JSON object  { "ok": true,  "command": "...", "result": ... }
//                                  or
//                                  { "ok": false, "command": "...", "error": "...",
//                                    "hint": "..." }
//   exit code      : 0 = success (even if "no results")
//                    2 = invalid params
//                    3 = network / upstream error
//                    4 = missing dependency
//
// ── Configuration (env vars) ───────────────────────────────────────────────
//   AGENT_REACH_PYTHON     Absolute path to the Python interpreter in the
//                          venv that has agent-reach installed.
//                          Default: <repo>/adapters/.venv/Scripts/python.exe
//                          (resolved relative to CARGO_MANIFEST_DIR at startup).
//   AGENT_REACH_RUNNER     Absolute path to the runner script.
//                          Default: <repo>/adapters/agent_reach_runner.py
//   AGENT_REACH_TIMEOUT    Per-call timeout in seconds. Default: 45.
//
// ── Why not just use `web_search`? ─────────────────────────────────────────
// `web_search` (SearXNG) is a search engine. It returns URLs with snippets
// — you still need a fetch step to read the page. This tool lets the model
// do BOTH the search and the fetch in one call (via jina_fetch), or pick
// the right channel for the source (YouTube, Bilibili, RSS, GitHub). The
// `web_search` tool remains useful for general discovery; `agent_reach` is
// the "I have a specific URL/source and I want the content" tool.

use std::env;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hydragent_types::{ToolResult, ToolStatus};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::tool_trait::Tool;

const DEFAULT_TIMEOUT_SECS: u64 = 45;
// Path resolution: we anchor to the workspace root (parent of `crates/`),
// not to the current working directory, so the tool works no matter where
// the user launches the binary from.
const RELATIVE_PY: &str = "adapters/.venv/Scripts/python.exe";
const RELATIVE_PY_NIX: &str = "adapters/.venv/bin/python";
const RELATIVE_RUNNER: &str = "adapters/agent_reach_runner.py";

pub struct AgentReachTool {
    python_path: PathBuf,
    runner_path: PathBuf,
    timeout: Duration,
}

impl AgentReachTool {
    /// Locate the runner and Python interpreter. We accept a workspace_root
    /// (parent of the crate dir) for the default search; explicit env vars
    /// always win.
    pub fn new(workspace_root: PathBuf) -> Self {
        let python_path = env::var("AGENT_REACH_PYTHON")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                // Prefer the venv inside `adapters/`. If the workspace
                // layout ever changes, callers can still override via env.
                let windows = workspace_root.join(RELATIVE_PY);
                if windows.exists() {
                    return windows;
                }
                workspace_root.join(RELATIVE_PY_NIX)
            });

        let runner_path = env::var("AGENT_REACH_RUNNER")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace_root.join(RELATIVE_RUNNER));

        let timeout_secs = env::var("AGENT_REACH_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        Self {
            python_path,
            runner_path,
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Spawn the Python runner, pipe JSON in, read JSON out.
    async fn run(&self, params: &Value) -> Result<Value, RunnerError> {
        // Pre-flight: both files must exist. A missing runner is a deployment
        // error, not a runtime error — we want a loud, specific failure.
        if !self.python_path.exists() {
            return Err(RunnerError::MissingPython(self.python_path.clone()));
        }
        if !self.runner_path.exists() {
            return Err(RunnerError::MissingRunner(self.runner_path.clone()));
        }

        let input = serde_json::to_string(params).unwrap_or_else(|_| "{}".to_string());

        let mut child = Command::new(&self.python_path)
            .arg(&self.runner_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Force utf-8 + no rich progress bar inside the runner subprocess.
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env_remove("PYTHONLEGACYWINDOWSSTDIO") // never let cp1252 sneak back in
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| RunnerError::Spawn(e.to_string()))?;

        // Write JSON to stdin, then close it so the runner's `sys.stdin.read()`
        // returns EOF promptly.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|e| RunnerError::StdinWrite(e.to_string()))?;
            // Drop closes stdin.
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| RunnerError::Wait(e.to_string()))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!(
            "agent_reach runner: exit={} stdout_bytes={} stderr_bytes={}",
            output.status,
            stdout.len(),
            stderr.len()
        );
        if !stderr.trim().is_empty() {
            // The runner only writes to stderr for unexpected errors after
            // it's already written a JSON line to stdout. We log it for the
            // operator but don't fail the call.
            warn!("agent_reach runner stderr: {}", stderr.trim());
        }

        // Parse stdout. The runner promises to write exactly one JSON line
        // (or none, if it crashed before emitting — rare).
        let parsed: Value = match serde_json::from_str(stdout.trim()) {
            Ok(v) => v,
            Err(e) => {
                return Err(RunnerError::BadJson {
                    parse_err: e.to_string(),
                    raw: stdout.chars().take(2000).collect(),
                });
            }
        };

        Ok(json!({
            "exit_code": output.status.code(),
            "runner_response": parsed,
        }))
    }
}

impl Default for AgentReachTool {
    fn default() -> Self {
        // Fall back to cwd-relative; the env var override is the supported
        // way to point at a non-default location.
        Self::new(PathBuf::from("."))
    }
}

#[async_trait]
impl Tool for AgentReachTool {
    fn name(&self) -> &str {
        "agent_reach"
    }

    fn description(&self) -> &str {
        "Bridge to the Agent-Reach ecosystem (Agent-Reach = installer+doctor for \
         upstream tools like Jina Reader, yt-dlp, feedparser, B站 search, GitHub \
         raw). Use this for structured reads from specific sources:\n\
         \n\
         Channels:\n\
         - jina_fetch: read any URL as clean Markdown (the universal channel)\n\
         - youtube:   fetch YouTube video metadata (+ optional subtitles)\n\
         - bilibili:  search Bilibili videos\n\
         - rss:       read an RSS/Atom feed\n\
         - github:    fetch a README or raw file from a public GitHub repo\n\
         - doctor:    show which Agent-Reach channels are active\n\
         \n\
         Prefer this over `web_search` when the LLM already has a specific URL, \
         a YouTube/Bilibili link, an RSS feed, or a GitHub repo in mind. For \
         open-ended discovery across the general web, use `web_search` first \
         and then `agent_reach` with `command=jina_fetch` on the result URL."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["jina_fetch", "youtube", "bilibili", "rss", "github", "doctor"],
                    "description": "Which Agent-Reach channel to invoke."
                },
                "url":      {"type": "string", "description": "For jina_fetch and rss: the target URL."},
                "query":    {"type": "string", "description": "For bilibili: search keywords."},
                "repo":     {"type": "string", "description": "For github: 'owner/name' (e.g. 'tokio-rs/tokio')."},
                "branch":   {"type": "string", "description": "For github: git ref (default: default branch)."},
                "path":     {"type": "string", "description": "For github: file path inside the repo (e.g. 'docs/index.md'). If omitted, the README is fetched."},
                "top_k":    {"type": "integer", "description": "For bilibili/rss: number of results to return (default 5/10)."},
                "max_chars":{"type": "integer", "description": "Max content length to return (default 12000 for jina, 8000 for youtube subs, 2000 for github)."},
                "subtitles":{"type": "boolean", "description": "For youtube: include auto-generated subtitles (default true). May be rate-limited by YouTube."},
                "selector": {"type": "string", "description": "For jina_fetch: optional CSS selector to extract a specific region of the page."}
            },
            "required": ["command"]
        }"#
    }

    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        // Read-only, no credentials, no side effects on local filesystem.
        // The subprocess is sandboxable later if we want to constrain it.
        hydragent_types::PermissionTier::AutoApprove
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = Instant::now();

        // Parse params (the contract — the LLM sends us a JSON object).
        let params: Value = match serde_json::from_str(params_json) {
            Ok(v) => v,
            Err(e) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: json!({
                        "error": format!("Invalid parameters: {}", e),
                        "hint": "Pass a JSON object with at least {\"command\": \"...\"}"
                    })
                    .to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Invalid JSON: {}", e)),
                };
            }
        };

        // Validate the command field early.
        let command = params
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim();
        if command.is_empty() {
            return ToolResult {
                call_id: String::new(),
                output_json: json!({
                    "error": "Missing 'command' parameter",
                    "hint": "Pass {\"command\": \"jina_fetch|youtube|bilibili|rss|github|doctor\"}"
                })
                .to_string(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some("Missing command".to_string()),
            };
        }

        // Run with a per-call timeout, but in a separate task so a hung
        // subprocess doesn't pin the whole orchestrator.
        let runner = self;
        let params_for_task = params.clone();
        let call_result = tokio::time::timeout(
            self.timeout,
            async move { runner.run(&params_for_task).await },
        )
        .await;

        let elapsed = start.elapsed().as_millis() as u32;

        let runner_response = match call_result {
            Ok(Ok(v)) => v, // success: {"exit_code", "runner_response"}
            Ok(Err(e)) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: json!({
                        "error": format!("{}", e),
                        "channel": command,
                    })
                    .to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: elapsed,
                    error_message: Some(format!("Runner: {}", e)),
                };
            }
            Err(_) => {
                return ToolResult {
                    call_id: String::new(),
                    output_json: json!({
                        "error": format!("Agent-Reach runner timed out after {}s", self.timeout.as_secs()),
                        "command": command,
                        "hint": "Increase AGENT_REACH_TIMEOUT or pick a lighter channel (e.g. doctor) to verify connectivity."
                    })
                    .to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: elapsed,
                    error_message: Some("Runner timeout".to_string()),
                };
            }
        };

        // Unwrap: the runner returns {"ok": bool, "command": ..., ...}. We
        // forward that as our tool output. The exit_code field tells the
        // caller whether the underlying python process exited cleanly even
        // when "ok" is true (it always will be when ok=true).
        let runner_resp = runner_response
            .get("runner_response")
            .cloned()
            .unwrap_or(Value::Null);
        let runner_ok = runner_resp
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        ToolResult {
            call_id: String::new(),
            output_json: serde_json::to_string(&runner_response).unwrap_or_default(),
            status: if runner_ok {
                ToolStatus::Success
            } else {
                ToolStatus::Failure
            },
            execution_ms: elapsed,
            error_message: if runner_ok {
                None
            } else {
                runner_resp
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            },
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Error type — human-readable, designed for the LLM to see in `error_message`.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
enum RunnerError {
    MissingPython(PathBuf),
    MissingRunner(PathBuf),
    Spawn(String),
    StdinWrite(String),
    Wait(String),
    BadJson { parse_err: String, raw: String },
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::MissingPython(p) => write!(
                f,
                "Python interpreter not found at {}. Set AGENT_REACH_PYTHON or \
                 install the venv at adapters/.venv with: \
                 python -m pip install -r adapters/requirements.txt",
                p.display()
            ),
            RunnerError::MissingRunner(p) => write!(
                f,
                "Agent-Reach runner script not found at {}. Set AGENT_REACH_RUNNER \
                 or restore adapters/agent_reach_runner.py from the repo.",
                p.display()
            ),
            RunnerError::Spawn(s) => write!(f, "Failed to spawn Python: {}", s),
            RunnerError::StdinWrite(s) => write!(f, "Failed to write to runner stdin: {}", s),
            RunnerError::Wait(s) => write!(f, "Failed waiting for runner: {}", s),
            RunnerError::BadJson { parse_err, raw } => write!(
                f,
                "Runner did not return valid JSON ({}). First 2000 chars: {}",
                parse_err,
                raw.chars().take(500).collect::<String>()
            ),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_paths_resolve_under_workspace() {
        // We can't easily change CWD in a unit test, but we can verify that
        // the new() constructor accepts any path and produces a struct.
        let tool = AgentReachTool::new(PathBuf::from("/tmp/fake-ws"));
        assert!(tool.python_path.ends_with("python.exe") || tool.python_path.ends_with("python"));
        assert!(tool.runner_path.ends_with("agent_reach_runner.py"));
    }

    #[test]
    fn test_missing_python_error_is_human_readable() {
        let err = RunnerError::MissingPython(PathBuf::from("/no/such/python"));
        let s = format!("{}", err);
        assert!(s.contains("Python interpreter not found"));
        assert!(s.contains("AGENT_REACH_PYTHON"));
    }

    #[test]
    fn test_missing_runner_error_suggests_restore() {
        let err = RunnerError::MissingRunner(PathBuf::from("/no/such/runner.py"));
        let s = format!("{}", err);
        assert!(s.contains("AGENT_REACH_RUNNER"));
        assert!(s.contains("agent_reach_runner.py"));
    }

    #[test]
    fn test_params_schema_contains_all_channels() {
        let tool = AgentReachTool::new(PathBuf::from("."));
        let schema = tool.params_schema();
        for ch in ["jina_fetch", "youtube", "bilibili", "rss", "github", "doctor"] {
            assert!(schema.contains(ch), "schema missing channel {}", ch);
        }
        // Required field is present
        assert!(schema.contains("\"required\""));
        assert!(schema.contains("\"command\""));
    }
}
