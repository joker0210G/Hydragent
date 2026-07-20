// Phase 7 / Track 7.4 - End-to-end task-completion benchmark CLI.
//
// Drives the real ReAct loop for every row in `task_bench_v1.jsonl`,
// grades the answer + tool outputs against the row's pass/fail rule,
// and prints a summary report. Optionally writes a JSON report.
//
// Usage:
//   cargo run -p hydragent-bench --release --bin task_bench -- \
//       --tasks tests/bench/task_bench_v1.jsonl \
//       --output reports/task_bench-v0.7.0.json
//
// Like the main `hydragent` binary, this picks up BRAIN_BASE /
// BRAIN_KEY / ACTIVE_PROVIDER / ACTIVE_MODEL / etc. from the same
// env-vars / `.env` it loads from `~/.hydragent/.env`.

use clap::Parser;
use hydragent_bench::{
    dataset::load_task_bench,
    runner::{run_task_bench, TaskExecutor, TaskOutcome},
};
use hydragent_core::config::AppConfig;
use hydragent_core::orchestrator::ActivePermissions;
use hydragent_core::paths;
use hydragent_core::react_loop;
use hydragent_memory::SessionStore;
use hydragent_model::registry::ProviderRegistry;
use hydragent_model::router::ModelRouter;
use hydragent_tools::registry::ToolRegistry;
use hydragent_tools::{
    code_execute::CodeExecuteTool,
    echo::EchoTool,
    file_read::FileReadTool,
    memory_search::MemorySearchTool,
    memory_store::MemoryStoreTool,
    url_fetch::UrlFetchTool,
    web_search::WebSearchTool,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(
    name = "task_bench",
    version,
    about = "End-to-end task-completion benchmark for the Hydragent ReAct loop"
)]
struct Cli {
    /// Path to task_bench_v1.jsonl
    #[arg(long)]
    tasks: PathBuf,

    /// Optional output JSON path for the full report (per-task + aggregate).
    #[arg(long)]
    output: Option<PathBuf>,

    /// Report version string (e.g. "v0.7.0")
    #[arg(long, default_value = "v0.7.0")]
    report_version: String,

    /// Per-task wall-clock timeout. If the ReAct loop doesn't finish in
    /// this many seconds the task is marked as a fail with reason "timeout".
    #[arg(long, default_value_t = 180)]
    task_timeout_secs: u64,

    /// Maximum ReAct steps per task. Forwarded to `run_react_loop`.
    #[arg(long, default_value_t = 10)]
    max_react_steps: u8,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    println!("Loading task bench from {}", cli.tasks.display());
    let tasks = load_task_bench(&cli.tasks)?;
    println!("  loaded {} tasks", tasks.len());

    // Credentials come ONLY from the encrypted Vault (never `.env`).
    // Mirrors `hydragent-core/src/main.rs` startup: load the vault and
    // surface its secrets as env vars so `AppConfig::load()` picks them
    // up through the standard `Environment::default()` source.
    load_vault_secrets();

    // Build the agent stack (mirrors `hydragent-core`'s main, minus the
    // things the benchmark doesn't need: dreaming, curator, scheduler,
    // gateway, swarm, audit chain).
    let app_config = AppConfig::load()?;
    let mut registry = build_provider_registry(&app_config);
    // Ollama models are auto-discovered from the running server (mirrors
    // cli_repl.rs). Without this, `resolve()` returns None for local tags
    // like `hf.co/unsloth/gemma-4-E4B-it-GGUF:IQ4_XS` and the router's
    // fallback splits the tag on '/' → 404. We sync discovered tags into
    // the YAML we loaded from, then reload so the registry knows them.
    if app_config.effective_brain_provider() == "ollama" {
        eprintln!("[task_bench] ollama provider detected, discovering models...");
        let ollama_base = std::env::var("BRAIN_BASE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("OLLAMA_API_BASE").ok())
            .unwrap_or_else(|| "http://localhost:11434".to_string());
        let yaml_path = if !app_config.model_providers_path.trim().is_empty() {
            PathBuf::from(&app_config.model_providers_path)
        } else {
            PathBuf::from(app_config.effective_model_providers_path())
        };
        eprintln!("[task_bench] ollama_base={} yaml_path={}", ollama_base, yaml_path.display());
        match tokio::time::timeout(Duration::from_millis(5000), hydragent_model::discover_ollama_models(&ollama_base)).await {
            Ok(Ok(tags)) if !tags.is_empty() => {
                eprintln!("[task_bench] discovered {} models", tags.len());
                if !yaml_path.exists() {
                    if let Some(parent) = yaml_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(
                        &yaml_path,
                        "providers:\n  - id: ollama\n    display_name: Ollama (Local)\n    kind: ollama\n    default_base_url: http://localhost:11434\n    auth_mode: none\n    supports_custom_models: true\n    supports_reasoning: true\n    supports_tools: true\n    models: []\n",
                    );
                }
                let _ = hydragent_model::sync_discovered_models_to_yaml(&yaml_path, &tags);
                match ProviderRegistry::load_from_yaml(&yaml_path) {
                    Ok(r) => {
                        eprintln!("[task_bench] reloaded registry, models={}", r.models(None).len());
                        for m in r.models(None) {
                            eprintln!("[task_bench]   model key id={} api_model_id={}", m.id, m.api_model_id);
                        }
                        registry = r;
                    }
                    Err(e) => eprintln!("[task_bench] reload FAILED: {}", e),
                }
            }
            _ => {
                eprintln!("[task_bench] discovery returned no models or timed out");
            }
        }
    }
    apply_role_overrides(&mut registry);
    let active_provider = app_config.effective_brain_provider();
    let active_model = app_config.effective_brain_model();
    // Pass the provider-prefixed ref so registry.resolve() matches the
    // registered alias. Ollama tags like `hf.co/unsloth/...` contain
    // slashes; without the `ollama/` prefix resolve() treats `hf.co` as
    // the provider, misses, and the router's fallback strips `hf.co/`
    // → 404. The api_model_id we hand the router is the bare tag.
    let wire_model = registry
        .resolve(&format!("{}/{}", active_provider, active_model), Some("chat"))
        .map(|r| r.api_model_id)
        .unwrap_or_else(|| active_model.clone());

    let brain_base = std::env::var("BRAIN_BASE").unwrap_or_default();
    let brain_key = app_config.effective_brain_key();
    let timeout_secs = if active_provider == "ollama" { 1800 } else { 180 };

    let brain_client: Arc<dyn hydragent_model::ModelProvider> = registry.build_provider(
        &active_provider,
        &brain_base,
        &brain_key,
        &wire_model,
        timeout_secs,
    );
    let model_router = Arc::new(ModelRouter::new_with_registry(
        brain_client,
        Arc::new(registry),
        wire_model.clone(),
        app_config.effective_brain_fallbacks(),
    ));

    // Session store + tool registry. The store lives in the standard
    // data dir; we don't isolate per-task because the suite intentionally
    // exercises cross-task memory (TB014 stores three facts, then expects
    // the right one back).
    let data_dir = hydragent_core::paths::data_dir();
    std::fs::create_dir_all(&data_dir)?;
    // SessionStore takes a plain filesystem path (matches main.rs), not a
    // `sqlite://` URL — the latter breaks on Windows backslash/colon paths.
    let db_path = data_dir.join("sessions.db");
    let store = Arc::new(SessionStore::new(&db_path.to_string_lossy()).await?);

    let workspace_dir = std::env::current_dir()?;
    let tool_registry = build_tool_registry(workspace_dir.clone(), store.clone());

    info!(
        provider = %active_provider,
        model = %wire_model,
        n_tasks = tasks.len(),
        "task_bench starting"
    );

    // Pre-build the ActivePermissions we share across all tasks.
    let active_permissions = ActivePermissions::default();

    let executor: TaskExecutor = build_executor(
        model_router,
        Arc::new(tool_registry),
        active_permissions,
        cli.task_timeout_secs,
        cli.max_react_steps,
    );

    let (results, scores) = run_task_bench(&tasks, &executor).await;

    print_report(&cli.report_version, &results, &scores);

    if let Some(out) = cli.output {
        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let report = json!({
            "version": cli.report_version,
            "model": wire_model,
            "provider": active_provider,
            "scores": scores,
            "results": results,
        });
        std::fs::write(&out, serde_json::to_string_pretty(&report)?)?;
        println!("\nWrote report to {}", out.display());
    }

    // ponytail: non-zero exit if pass rate < 50%, so CI can gate on it.
    if scores.total > 0 && scores.pass_rate < 0.5 {
        std::process::exit(2);
    }
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("HYDRAGENT_TASK_BENCH_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,hydragent_bench=info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

// ─── Vault-backed credentials ────────────────────────────────────────
//
// The benchmark never reads API keys from `.env` or plaintext. They live
// only in the encrypted Vault (set via `hydragent vault set <scope> <key>`
// or the `/provider add` REPL command). We mirror `main.rs` startup:
// unlock the vault, then surface its secrets as env vars so the rest of
// the stack (AppConfig + ModelRouter) resolves them through the normal
// `Environment::default()` source. No key ever touches a config file.
fn load_vault_secrets() {
    let vault_path = paths::data_dir().join("vault/.hydravault");
    let vault = hydragent_vault::Vault::new(vault_path);
    if !vault.exists() {
        return;
    }
    let passphrase = std::env::var("HYDRAGENT_VAULT_PASSPHRASE").unwrap_or_default();
    let Ok(secrets) = vault.load(&passphrase) else {
        eprintln!(
            "[task_bench] vault exists but could not be unlocked (set HYDRAGENT_VAULT_PASSPHRASE). \
             Skipping vault credentials."
        );
        return;
    };
    // OpenRouter key rotation list + legacy aliases.
    for scope in ["OPENROUTER_API_KEYS", "BRAIN_OPENROUTER_KEY", "OPENROUTER_API_KEY"] {
        if let Some(s) = secrets.get(scope) {
            std::env::set_var(scope, s.expose_secret());
        }
    }
    // Active model/provider selection (vault wins over empty defaults).
    for scope in ["PRIMARY_MODEL", "BRAIN_MODEL", "ACTIVE_MODEL"] {
        if let Some(s) = secrets.get(scope) {
            let v = s.expose_secret();
            if !v.trim().is_empty() {
                std::env::set_var(scope, v);
            }
        }
    }
    if let Some(s) = secrets.get("ACTIVE_PROVIDER") {
        let v = s.expose_secret();
        if !v.trim().is_empty() {
            std::env::set_var("ACTIVE_PROVIDER", v);
        }
    }
}

// ─── Provider registry construction ──────────────────────────────────
//
// Mirrors `crates/hydragent-core/src/main.rs::load_provider_registry`
// but is duplicated here so the bin doesn't have to reach into the
// private `pub(crate)` helper. Kept intentionally minimal — only the
// "load YAML if present, fall back to built-in defaults" path.

fn build_provider_registry(app_config: &AppConfig) -> ProviderRegistry {
    if !app_config.model_providers_path.trim().is_empty() {
        if let Ok(r) = ProviderRegistry::load_from_yaml(&app_config.model_providers_path) {
            return r;
        }
    }
    let default_path = app_config.effective_model_providers_path();
    if std::path::Path::new(&default_path).exists() {
        if let Ok(r) = ProviderRegistry::load_from_yaml(&default_path) {
            return r;
        }
    }
    if std::path::Path::new("config/model_providers.yaml").exists() {
        if let Ok(r) = ProviderRegistry::load_from_yaml("config/model_providers.yaml") {
            return r;
        }
    }
    ProviderRegistry::builtin_default()
}

fn apply_role_overrides(registry: &mut ProviderRegistry) {
    for (key, value) in std::env::vars() {
        let Some(role) = key.strip_prefix("DEFAULT_MODEL_") else {
            continue;
        };
        registry.set_role_default(role.to_lowercase(), value);
    }
}

fn build_tool_registry(workspace_dir: PathBuf, store: Arc<SessionStore>) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(EchoTool);
    reg.register(FileReadTool::new(workspace_dir.clone()));
    reg.register(CodeExecuteTool::new(workspace_dir));
    reg.register(UrlFetchTool::new());
    reg.register(WebSearchTool::new());
    reg.register(MemoryStoreTool::new(store.clone()));
    reg.register(MemorySearchTool::new(store));
    // Deliberately NOT registering security tools, schedule_task, skill_*,
    // rss_subscribe, standing_orders, user_profile, agent_reach. The task
    // suite doesn't exercise them and adding them would inflate the
    // system prompt without improving signal.
    reg
}

// ─── Executor wiring ────────────────────────────────────────────────

fn build_executor(
    model_router: Arc<ModelRouter>,
    tool_registry: Arc<ToolRegistry>,
    active_permissions: ActivePermissions,
    task_timeout_secs: u64,
    max_react_steps: u8,
) -> TaskExecutor {
    Box::new(move |task| {
        let router = model_router.clone();
        let registry = tool_registry.clone();
        let perms = active_permissions.clone();
        let prompt = task.prompt.clone();
        let id = task.id.clone();
        Box::pin(async move {
            run_single_task(router, registry, perms, &id, &prompt, task_timeout_secs, max_react_steps).await
        })
    })
}

/// Drive `run_react_loop` once with auto-approve of permission prompts
/// and capture both the streamed answer and the tool results.
async fn run_single_task(
    model_router: Arc<ModelRouter>,
    tool_registry: Arc<ToolRegistry>,
    active_permissions: ActivePermissions,
    task_id: &str,
    prompt: &str,
    task_timeout_secs: u64,
    max_react_steps: u8,
) -> TaskOutcome {
    let start = Instant::now();
    // Unique page id per task so the loop's internal state and any
    // auto-approve bookkeeping can't bleed between tasks.
    let page_id = format!("task_bench-{}", uuid::Uuid::new_v4());
    let channel_id = "bench".to_string();
    let user_id = "system".to_string();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(256);
    let perms_for_approver = active_permissions.clone();
    // Shared set of tool ids the loop actually invoked this task. The
    // `response.tool_call` stream message (emitted for every tier) is the
    // source of truth; `ToolResult` doesn't carry a tool id.
    let invoked = Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::<String>::new()));
    let invoked_for_approver = invoked.clone();
    let approver = tokio::spawn(async move {
        // Drain the same channel the loop writes to; whenever we see a
        // `response.permission_request`, immediately approve it. This is
        // what makes the benchmark headless: no human in the loop.
        while let Some(line) = rx.recv().await {
            let Ok(v): Result<Value, _> = serde_json::from_str(&line) else { continue };
            match v.get("method").and_then(|m| m.as_str()) {
                Some("response.permission_request") => {
                    if let Some(req_id) = v
                        .get("params")
                        .and_then(|p| p.get("request_id"))
                        .and_then(|r| r.as_str())
                    {
                        let req_id = req_id.to_string();
                        let mut pending = perms_for_approver.pending.lock().await;
                        if let Some(tx) = pending.remove(&req_id) {
                            let _ = tx.send(true);
                        }
                    }
                }
                Some("response.tool_call") => {
                    if let Some(tool_id) = v
                        .get("params")
                        .and_then(|p| p.get("tool_id"))
                        .and_then(|t| t.as_str())
                    {
                        invoked_for_approver.lock().await.insert(tool_id.to_string());
                    }
                }
                _ => {}
            }
        }
    });

    // run_react_loop returns (final_answer, Vec<ToolResult>).
    let work = react_loop::run_react_loop(
        &page_id,
        &channel_id,
        &user_id,
        prompt,
        Vec::new(),
        Vec::new(),
        None,
        None,
        model_router,
        tool_registry,
        max_react_steps,
        tx,
        active_permissions,
        None, // skill_library: skip for the benchmark
    );

    let outcome = match tokio::time::timeout(Duration::from_secs(task_timeout_secs), work).await {
        Ok(Ok((answer, tool_results))) => {
            let tools_invoked: Vec<String> =
                invoked.lock().await.iter().cloned().collect();
            TaskOutcome {
                answer,
                tool_outputs: tool_outputs_blob(&tool_results),
                tools_invoked,
                duration_ms: start.elapsed().as_millis() as u64,
            }
        }
        Ok(Err(e)) => {
            error!(task_id, "react_loop error: {e}");
            TaskOutcome {
                answer: format!("[task_bench error: {e}]"),
                tool_outputs: String::new(),
                tools_invoked: Vec::new(),
                duration_ms: start.elapsed().as_millis() as u64,
            }
        }
        Err(_elapsed) => {
            error!(task_id, "react_loop timed out after {task_timeout_secs}s");
            TaskOutcome {
                answer: format!("[task_bench timeout after {task_timeout_secs}s]"),
                tool_outputs: String::new(),
                tools_invoked: Vec::new(),
                duration_ms: start.elapsed().as_millis() as u64,
            }
        }
    };

    approver.abort();
    let _ = approver.await;
    outcome
}

/// Stringify every tool result (output_json + any error_message) into
/// one blob the grader can scan. We keep this dumb and JSON-only so
/// test expectations stay readable.
fn tool_outputs_blob(results: &[hydragent_types::ToolResult]) -> String {
    let mut out = String::new();
    for r in results {
        if !r.output_json.is_empty() {
            out.push_str(&r.output_json);
            out.push('\n');
        }
        if let Some(err) = &r.error_message {
            out.push_str(err);
            out.push('\n');
        }
    }
    out
}

// ─── Reporting ──────────────────────────────────────────────────────

fn print_report(version: &str, results: &[hydragent_bench::runner::TaskResult], scores: &hydragent_bench::runner::TaskBenchScores) {
    println!();
    println!("== TASK-BENCH {} ==", version);
    println!(
        "  pass: {}/{}  ({:.1}%)",
        scores.passed,
        scores.total,
        scores.pass_rate * 100.0
    );
    println!();
    println!("  By difficulty:");
    for (diff, bucket) in &scores.by_difficulty {
        println!(
            "    {:<8} {}/{}  ({:.1}%)",
            diff,
            bucket.passed,
            bucket.total,
            bucket.pass_rate * 100.0
        );
    }
    println!();
    println!("  Per-task:");
    for r in results {
        let mark = if r.passed { "PASS" } else { "FAIL" };
        println!(
            "    [{}] {:<7} {} ({:?}) — tools={:?} dur={}ms",
            r.id,
            mark,
            r.difficulty,
            r.failures,
            r.outcome.tools_invoked,
            r.outcome.duration_ms
        );
    }
}
