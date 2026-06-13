// crates/hydragent-core/src/main.rs
pub mod orchestrator;
pub mod react_loop;
pub mod session;
pub mod logger;
pub mod config;
pub mod dream;
pub mod strategy;
pub mod swarm_runner;

use clap::Parser;
use std::sync::Arc;
use std::path::PathBuf;
use hydragent_memory::SessionStore;
use hydragent_tools::registry::ToolRegistry;
use hydragent_tools::echo::EchoTool;
use hydragent_tools::web_search::WebSearchTool;
use hydragent_tools::file_read::FileReadTool;
use hydragent_tools::memory_store::MemoryStoreTool;
use hydragent_tools::memory_search::MemorySearchTool;
use hydragent_tools::memory_forget::MemoryForgetTool;
use tracing::{info, error};

struct SandboxedTool {
    name: String,
    description: String,
    schema: String,
    wasm_tool: hydragent_sandbox::WasmTool,
    tier: hydragent_types::PermissionTier,
}

#[async_trait::async_trait]
impl hydragent_tools::tool_trait::Tool for SandboxedTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn params_schema(&self) -> &str {
        &self.schema
    }
    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        self.tier
    }
    async fn execute(&self, params_json: &str) -> hydragent_types::ToolResult {
        match self.wasm_tool.execute(params_json).await {
            Ok(res) => res,
            Err(e) => hydragent_types::ToolResult {
                call_id: "".to_string(),
                output_json: "".to_string(),
                status: hydragent_types::ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some(format!("Sandbox execution failed: {}", e)),
            }
        }
    }
}


#[derive(Parser, Debug)]
#[command(name = "hydragent", author, version, about = "Hydragent AI Agent core runtime")]
struct Args {
    /// Resumes or starts a specific chat session by ID
    #[arg(short, long)]
    session: Option<String>,

    /// Lists all past conversation sessions stored in the SQLite database and exits
    #[arg(long)]
    list_sessions: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Manage long-term memory
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Manage local vector embeddings
    Embed {
        #[command(subcommand)]
        action: EmbedAction,
    },
    /// Manage the encrypted credential vault
    Vault {
        #[command(subcommand)]
        action: VaultAction,
    },
    /// Send a real prompt to the live brain and stream the reply
    /// (a real-time end-to-end test of the swappable BRAIN_* config)
    TestBrain {
        /// The prompt to send. Default: a one-liner that asks the model
        /// to introduce itself so you can confirm the brain is wired up.
        #[arg(default_value = "In one sentence, who are you and which model are you?")]
        prompt: String,
    },
}

#[derive(clap::Subcommand, Debug)]
enum VaultAction {
    /// Initialize a new encrypted vault
    Init,
    /// Store a credential in the vault
    Set {
        scope: String,
        value: String,
    },
    /// Retrieve a credential from the vault
    Get {
        scope: String,
    },
    /// List all credential scopes in the vault
    List,
    /// Delete a credential from the vault
    Delete {
        scope: String,
    },
}

#[derive(clap::Subcommand, Debug)]
enum MemoryAction {
    /// List all stored memories
    List,
    /// Clear all stored memories
    Clear,
}

#[derive(clap::Subcommand, Debug)]
enum EmbedAction {
    /// Calculate cosine similarity between two sentences
    Compare {
        text1: String,
        text2: String,
    },
}

#[tokio::main]
async fn main() {
    let start_time = std::time::Instant::now();
    let args = Args::parse();

    // Load configuration
    let mut app_config = config::AppConfig::load().unwrap_or_else(|e| {
        eprintln!("Failed to load configuration: {}", e);
        std::process::exit(1);
    });

    // Load secrets from encrypted vault if configured and passphrase is provided
    let vault_path = std::path::PathBuf::from(&app_config.data_dir).join("vault/.hydravault");
    if vault_path.exists() {
        if let Ok(passphrase) = std::env::var("HYDRAGENT_VAULT_PASSPHRASE") {
            let vault = hydragent_vault::Vault::new(vault_path);
            match vault.load(&passphrase) {
                Ok(secrets) => {
                    tracing::info!("Loaded secrets from cryptographic Vault.");
                    if let Some(keys) = secrets.get("OPENROUTER_API_KEYS") {
                        app_config.openrouter_api_keys = keys.expose_secret().to_string();
                    }
                    if let Some(model) = secrets.get("PRIMARY_MODEL") {
                        // Legacy key — re-export as BRAIN_MODEL so it seeds
                        // the new effective_brain_model() helper.
                        app_config.brain_model = model.expose_secret().to_string();
                    }
                    if let Some(model) = secrets.get("BRAIN_MODEL") {
                        app_config.brain_model = model.expose_secret().to_string();
                    }
                    // Load any other configuration keys from Vault dynamically
                    for (scope, secret) in secrets {
                        std::env::set_var(scope, secret.expose_secret());
                    }
                }
                Err(e) => {
                    eprintln!("Failed to decrypt cryptographic Vault: incorrect passphrase. Error: {}", e);
                }
            }
        } else {
            eprintln!("Cryptographic Vault exists, but HYDRAGENT_VAULT_PASSPHRASE is not set. Bypassing Vault decryption.");
        }
    }

    // Initialize the logger using configured values
    logger::init_logger(&app_config.log_format, &app_config.log_level);

    if let Some(Commands::Embed { action }) = &args.command {
        match action {
            EmbedAction::Compare { text1, text2 } => {
                let paths = hydragent_embed::ensure_model_downloaded(&app_config.data_dir)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("Failed to download/verify embedding model: {}", e);
                        std::process::exit(1);
                    });
                let embedder = hydragent_embed::LocalEmbedder::new(&paths.model_path, &paths.tokenizer_path)
                    .unwrap_or_else(|e| {
                        eprintln!("Failed to initialize local embedder: {}", e);
                        std::process::exit(1);
                    });
                let v1 = embedder.embed_text(text1).unwrap_or_else(|e| {
                    eprintln!("Failed to embed text1: {}", e);
                    std::process::exit(1);
                });
                let v2 = embedder.embed_text(text2).unwrap_or_else(|e| {
                    eprintln!("Failed to embed text2: {}", e);
                    std::process::exit(1);
                });
                let sim = hydragent_embed::cosine_similarity(&v1, &v2);
                println!("Cosine similarity: {:.4}", sim);
            }
        }
        return;
    }

    fn get_passphrase() -> anyhow::Result<String> {
        if let Ok(val) = std::env::var("HYDRAGENT_VAULT_PASSPHRASE") {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                return Ok(trimmed);
            }
        }

        let password = rpassword::prompt_password("Enter vault passphrase: ")?;
        let trimmed = password.trim().to_string();
        if trimmed.is_empty() {
            return Err(anyhow::anyhow!("Passphrase cannot be empty"));
        }
        Ok(trimmed)
    }

    if let Some(Commands::Vault { action }) = &args.command {
        let vault_path = std::path::PathBuf::from(&app_config.data_dir).join("vault/.hydravault");
        let vault = hydragent_vault::Vault::new(vault_path);

        match action {
            VaultAction::Init => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                vault.init(&passphrase).unwrap_or_else(|e| {
                    eprintln!("Failed to initialize vault: {}", e);
                    std::process::exit(1);
                });
                println!("Vault initialized successfully.");
            }
            VaultAction::Set { scope, value } => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let mut secrets = if vault.exists() {
                    vault.load(&passphrase).unwrap_or_else(|e| {
                        eprintln!("Failed to load vault: {}", e);
                        std::process::exit(1);
                    })
                } else {
                    eprintln!("Vault does not exist. Please initialize it first using `vault init`.");
                    std::process::exit(1);
                };
                secrets.insert(scope.clone(), hydragent_vault::TaintedString::new(value.clone()));
                vault.save(&passphrase, &secrets).unwrap_or_else(|e| {
                    eprintln!("Failed to save secret to vault: {}", e);
                    std::process::exit(1);
                });
                println!("Secret set successfully for scope: {}", scope);
            }
            VaultAction::Get { scope } => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let secrets = vault.load(&passphrase).unwrap_or_else(|e| {
                    eprintln!("Failed to load vault: {}", e);
                    std::process::exit(1);
                });
                if let Some(secret) = secrets.get(scope) {
                    println!("{}", secret.expose_secret());
                } else {
                    eprintln!("Secret not found for scope: {}", scope);
                    std::process::exit(1);
                }
            }
            VaultAction::List => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let secrets = vault.load(&passphrase).unwrap_or_else(|e| {
                    eprintln!("Failed to load vault: {}", e);
                    std::process::exit(1);
                });
                println!("Stored secret scopes:");
                for scope in secrets.keys() {
                    println!("  - {}", scope);
                }
            }
            VaultAction::Delete { scope } => {
                let passphrase = get_passphrase().unwrap_or_else(|e| {
                    eprintln!("Error getting passphrase: {}", e);
                    std::process::exit(1);
                });
                let mut secrets = vault.load(&passphrase).unwrap_or_else(|e| {
                    eprintln!("Failed to load vault: {}", e);
                    std::process::exit(1);
                });
                if secrets.remove(scope).is_some() {
                    vault.save(&passphrase, &secrets).unwrap_or_else(|e| {
                        eprintln!("Failed to save vault: {}", e);
                        std::process::exit(1);
                    });
                    println!("Secret deleted successfully for scope: {}", scope);
                } else {
                    eprintln!("Secret not found for scope: {}", scope);
                    std::process::exit(1);
                }
            }
        }
        return;
    }

    let startup_duration = start_time.elapsed();
    info!("🐉 Hydragent startup latency: {:?}", startup_duration);

    // Build SQLite DB filepath
    let db_path = format!("{}/sessions.db", app_config.data_dir);
    let mut store = SessionStore::new(&db_path).await.unwrap_or_else(|e| {
        error!("Failed to initialize session database: {}", e);
        std::process::exit(1);
    });

    // Apply the LRU eviction cap from config (no-op when usize::MAX).
    store.with_max_memories(app_config.max_semantic_memories);
    if app_config.max_semantic_memories < usize::MAX {
        info!(
            cap = app_config.max_semantic_memories,
            "🧹 Memory LRU eviction enabled"
        );
    }
    let store = Arc::new(store);

    // ── The "brain" (single live provider) ────────────────────────────
    //
    // The agent has one brain, swappable via 4 env vars:
    //   BRAIN_BASE     = https://api.together.xyz/v1   (or openai, openrouter, ollama, ...)
    //   BRAIN_KEY      = sk-...                       (empty for local providers)
    //   BRAIN_MODEL    = meta-llama/Llama-3-70b-chat-hf
    //   BRAIN_FALLBACKS= smaller-model1,smaller-model2
    //
    // Backward compat: if BRAIN_BASE is unset but OPENROUTER_API_KEYS is set,
    // we use OpenRouter's URL. If BRAIN_MODEL is unset we use PRIMARY_MODEL,
    // and BRAIN_FALLBACKS falls back to FALLBACK_MODELS.
    let brain_base = app_config.effective_brain_base();
    let brain_key = app_config.effective_brain_key();
    let brain_model = app_config.effective_brain_model();
    let brain_fallbacks = app_config.effective_brain_fallbacks();

    if brain_base.is_empty() {
        eprintln!(
            "🤔 I don't know where to connect. Set `BRAIN_BASE` in `.env`.\n\
             Examples:\n\
               BRAIN_BASE=https://api.openai.com/v1\n\
               BRAIN_BASE=https://openrouter.ai/api/v1\n\
               BRAIN_BASE=http://localhost:11434/v1  (Ollama in OpenAI-compat mode)\n\
             Or set `OPENROUTER_API_KEYS` for backward compatibility."
        );
        std::process::exit(1);
    }

    if !brain_key.is_empty() && brain_key.contains("9b9c8f09436e") {
        tracing::warn!(
            "⚠️ Default placeholder API key detected. Replace it with a valid key in `.env`."
        );
    }

    info!(
        base = brain_base.as_str(),
        primary = brain_model.as_str(),
        fallbacks = ?brain_fallbacks,
        "🧠 Building live brain"
    );

    let brain_config = hydragent_model::custom_openai::CustomProviderConfig {
        base_url: brain_base.clone(),
        api_key: brain_key,
        default_model: brain_model.clone(),
        provider_label: "brain".to_string(),
        // 180s gives slow LLM providers (tokenrouter rate-limits, long
        // ReAct loops with multiple tool calls) enough headroom to
        // complete without aborting. The test harness uses
        // `TIMEOUT_LLM=90.0` / `TIMEOUT_LLM_LONG=180.0`; matching the
        // upper bound here keeps the rust side from being the bottleneck.
        timeout: std::time::Duration::from_secs(180),
        max_retries: 3,
    };
    let brain_client: Arc<dyn hydragent_model::ModelProvider> =
        Arc::new(hydragent_model::custom_openai::CustomOpenAIClient::new(brain_config));

    let model_router = Arc::new(hydragent_model::router::ModelRouter::new(
        brain_client,
        brain_model,
        brain_fallbacks,
    ));

    // ── `test-brain` subcommand ─────────────────────────────────────────
    //
    // Real-time practical test: stream a prompt through the live brain and
    // print the response. Exercises the full BRAIN_* → ModelRouter →
    // CustomOpenAIClient → SSE pipeline. This is the quickest way to
    // confirm a freshly pasted key actually works end-to-end.
    if let Some(Commands::TestBrain { prompt }) = &args.command {
        println!("------------------------------------------------------------------------");
        println!("  🧠 Hydragent live-brain test");
        println!("  base     : {}", brain_base);
        println!("  primary  : {}", app_config.effective_brain_model());
        println!("  fallbacks: {:?}", app_config.effective_brain_fallbacks());
        println!("  prompt   : {}", prompt);
        println!("------------------------------------------------------------------------");
        println!();

        use hydragent_model::openrouter::ChatMessage;
        use tokio::sync::mpsc;
        let (tx, mut rx) = mpsc::channel::<String>(256);
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: prompt.clone(),
        }];

        // Spawn the stream consumer so we can print tokens as they arrive.
        let printer = tokio::spawn(async move {
            let mut stdout = tokio::io::stdout();
            use tokio::io::AsyncWriteExt;
            while let Some(token) = rx.recv().await {
                let _ = stdout.write_all(token.as_bytes()).await;
                let _ = stdout.flush().await;
            }
        });

        let started = std::time::Instant::now();
        match model_router.chat_stream(messages, tx, None).await {
            Ok((content, used_model)) => {
                let _ = printer.await;
                let elapsed = started.elapsed();
                println!();
                println!();
                println!("------------------------------------------------------------------------");
                println!(
                    "  ✅ Brain spoke (model={}, {:.2}s, {} chars)",
                    used_model,
                    elapsed.as_secs_f64(),
                    content.chars().count()
                );
                println!("------------------------------------------------------------------------");
            }
            Err(e) => {
                eprintln!();
                eprintln!();
                eprintln!("------------------------------------------------------------------------");
                eprintln!("  ❌ Brain failed: {}", e);
                eprintln!("------------------------------------------------------------------------");
                std::process::exit(1);
            }
        }
        return;
    }

    if args.list_sessions {
        println!("------------------------------------------------------------------------");
        println!("  🐉 Hydragent Page History");
        println!("  Database: {}", db_path);
        println!("------------------------------------------------------------------------");
        match store.list_pages().await {
            Ok(pages) => {
                if pages.is_empty() {
                    println!("  No active Pages found.");
                } else {
                    println!("  {:<36} | {:<20} | {:<5}", "Page ID", "Last Active", "Turns");
                    println!("  ----------------------------------------------------------------------");
                    for (page_id, _, last_active, turn_count) in pages {
                        let dt = chrono::DateTime::from_timestamp(last_active / 1000, 0)
                            .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                            .unwrap_or_else(|| "Unknown".to_string());
                        println!("  {:<36} | {:<20} | {:<5}", page_id, dt, turn_count);
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to read database page list: {}", e);
            }
        }
        println!("------------------------------------------------------------------------");
        return;
    }

    if let Some(cmd) = args.command {
        match cmd {
            Commands::Memory { action } => {
                match action {
                    MemoryAction::List => {
                        println!("------------------------------------------------------------------------");
                        println!("  🐉 Hydragent Semantic Memories");
                        println!("  Database: {}", db_path);
                        println!("------------------------------------------------------------------------");
                        match store.list_memories().await {
                            Ok(memories) => {
                                if memories.is_empty() {
                                    println!("  No semantic memories found.");
                                } else {
                                    println!("  {:<36} | {:<5} | {:<50}", "Memory ID", "Imp.", "Content");
                                    println!("  ----------------------------------------------------------------------");
                                    for mem in memories {
                                        println!("  {:<36} | {:<5} | {}", mem.id, mem.importance, mem.content);
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to read database memory list: {}", e);
                            }
                        }
                        println!("------------------------------------------------------------------------");
                    }
                    MemoryAction::Clear => {
                        println!("Clearing all stored memories...");
                        match store.clear_all_memories().await {
                            Ok(_) => println!("Success: All semantic memories and search indexes cleared."),
                            Err(e) => eprintln!("Error clearing memories: {}", e),
                        }
                    }
                }
            }
            Commands::Embed { .. } | Commands::Vault { .. } | Commands::TestBrain { .. } => unreachable!(),
        }
        return;
    }

    info!("Hydragent starting up with config: {:?}", app_config);

    // Spawn dreaming background worker if enabled
    if app_config.enable_dreaming {
        let store_clone = store.clone();
        let model_router_clone = model_router.clone();
        let interval_secs = app_config.dreaming_interval_sec;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                info!("Dreaming worker waking up...");
                let cycle = dream::run_dream_cycle(
                    store_clone.clone(),
                    model_router_clone.clone(),
                ).await;
                match cycle {
                    Ok(stats) => {
                        if stats.messages_processed > 0 {
                            info!(?stats, "Dream cycle completed successfully");
                        }
                    }
                    Err(e) => {
                        error!("Dream cycle error: {}", e);
                    }
                }
            }
        });
    }

    // Initialize Tool Registry
    let workspace_dir = std::env::var("WORKSPACE_DIR")
        .unwrap_or_else(|_| ".".to_string());
    
    let mut registry = ToolRegistry::new();
    let wasm_engine = hydragent_sandbox::create_sandbox_engine().unwrap();
    let sandbox_tools_dir = PathBuf::from("./sandbox/tools");

    let enforce_sandbox = std::env::var("ENFORCE_SANDBOX")
        .map(|v| v.trim().to_lowercase() == "true")
        .unwrap_or(false);

    // Load echo WASM
    let echo_wasm_path = sandbox_tools_dir.join("echo.wasm");
    if echo_wasm_path.exists() {
        let wasm_tool = hydragent_sandbox::WasmTool::load(
            &wasm_engine,
            &echo_wasm_path,
            hydragent_sandbox::ResourceLimits::default(),
            None,
        ).unwrap();
        registry.register(SandboxedTool {
            name: "echo".to_string(),
            description: "Echoes back the input message inside a WASM sandbox.".to_string(),
            schema: r#"{"type":"object","properties":{"call_id":{"type":"string"},"message":{"type":"string"}},"required":["call_id","message"]}"#.to_string(),
            wasm_tool,
            tier: hydragent_types::PermissionTier::AutoApprove,
        });
        info!("Registered sandboxed echo tool.");
    } else {
        if enforce_sandbox {
            panic!("Security Violation: ENFORCE_SANDBOX is enabled but sandboxed tool 'echo.wasm' is missing at {:?}", echo_wasm_path);
        } else {
            tracing::warn!("Warning: Bypassing WASM sandbox for 'echo' tool, registering local native fallback.");
            registry.register(EchoTool);
        }
    }

    registry.register(WebSearchTool::new());

    // Load file_read WASM
    let file_read_wasm_path = sandbox_tools_dir.join("file_read.wasm");
    if file_read_wasm_path.exists() {
        let wasm_tool = hydragent_sandbox::WasmTool::load(
            &wasm_engine,
            &file_read_wasm_path,
            hydragent_sandbox::ResourceLimits::default(),
            Some(PathBuf::from(&workspace_dir)),
        ).unwrap();
        registry.register(SandboxedTool {
            name: "file_read".to_string(),
            description: "Reads a file in the workspace securely inside a WASM sandbox.".to_string(),
            schema: r#"{"type":"object","properties":{"call_id":{"type":"string"},"path":{"type":"string"}},"required":["call_id","path"]}"#.to_string(),
            wasm_tool,
            tier: hydragent_types::PermissionTier::Prompt,
        });
        info!("Registered sandboxed file_read tool.");
    } else {
        if enforce_sandbox {
            panic!("Security Violation: ENFORCE_SANDBOX is enabled but sandboxed tool 'file_read.wasm' is missing at {:?}", file_read_wasm_path);
        } else {
            tracing::warn!("Warning: Bypassing WASM sandbox for 'file_read' tool, registering local native fallback.");
            registry.register(FileReadTool::new(PathBuf::from(&workspace_dir)));
        }
    }
    registry.register(MemoryStoreTool::new(store.clone()));
    registry.register(MemorySearchTool::new(store.clone()));
    registry.register(MemoryForgetTool::new(store.clone()));
    registry.register(hydragent_tools::standing_orders::SoulTool::new(PathBuf::from("./config")));
    registry.register(hydragent_tools::user_profile::UserProfileTool::new(PathBuf::from("./config")));
    // Initialize Gateway Router
    let gateway_router = Arc::new(hydragent_gateway::GatewayRouter::new());

    // Initialize Heartbeat Engine
    let heartbeat = Arc::new(hydragent_scheduler::HeartbeatEngine::new(gateway_router.clone()));

    // Initialize Work IQ Engine
    let work_iq = hydragent_scheduler::work_iq::WorkIqEngine::new(
        store.pool().clone(),
        heartbeat.clone(),
        model_router.clone(),
    );

    // Create scheduler executor with OnceCell to break dependency cycle
    let registry_cell = Arc::new(tokio::sync::OnceCell::<Arc<ToolRegistry>>::new());
    let store_clone = store.clone();
    let model_router_clone = model_router.clone();
    let heartbeat_clone = heartbeat.clone();
    let work_iq_clone = work_iq.clone();
    let max_react_steps = app_config.max_react_steps;
    let registry_cell_clone = registry_cell.clone();

    let executor = Arc::new(move |job: hydragent_types::CronJob| {
        let _store = store_clone.clone();
        let model_router = model_router_clone.clone();
        let heartbeat = heartbeat_clone.clone();
        let registry_cell = registry_cell_clone.clone();
        let work_iq = work_iq_clone.clone();
        let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> = Box::pin(async move {
            if job.task_type == "react_loop" {
                let mut prompt = job.task_params.clone();
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&job.task_params) {
                    if let Some(obj) = val.as_object() {
                        if let Some(p) = obj.get("prompt").or_else(|| obj.get("query")).or_else(|| obj.get("content")).or_else(|| obj.get("task")) {
                            if let Some(s) = p.as_str() {
                                prompt = s.to_string();
                            }
                        }
                    }
                }
                let page_id = format!("cron-{}", job.id);
                let history_messages = vec![];
                let retrieved_memories = vec![];
                let user_profile = std::fs::read_to_string("./config/USER.md").ok();
                let soul_guidelines = std::fs::read_to_string("./config/SOUL.md").ok();
                
                let (tx, mut rx) = tokio::sync::mpsc::channel(100);
                let active_perms = orchestrator::ActivePermissions::default();
                let page_id_clone = page_id.clone();
                let prompt_clone = prompt.clone();
                
                let registry = registry_cell.get().expect("Registry not set").clone();
                tokio::spawn(async move {
                    let _ = crate::react_loop::run_react_loop(
                        &page_id_clone,
                        "cli",
                        "system",
                        &prompt_clone,
                        history_messages,
                        retrieved_memories,
                        user_profile,
                        soul_guidelines,
                        model_router,
                        registry,
                        max_react_steps,
                        tx,
                        active_perms,
                    ).await;
                });
                
                let mut accumulated_tokens = String::new();
                while let Some(line) = rx.recv().await {
                    if let Ok(msg_val) = serde_json::from_str::<serde_json::Value>(&line) {
                        if msg_val.get("method").and_then(|m| m.as_str()) == Some("response.token") {
                            if let Some(token) = msg_val.get("params").and_then(|p| p.get("token")).and_then(|t| t.as_str()) {
                                accumulated_tokens.push_str(token);
                            }
                        }
                    }
                }
                
                if !accumulated_tokens.is_empty() {
                    let _ = heartbeat.push(job.target_channel_id, page_id, accumulated_tokens).await;
                }
            } else if job.task_type == "heartbeat" {
                // Phase 4 G6: Proactive push — relay a static or pre-summarized
                // string to the target channel without spinning the LLM.
                // Renamed from "message" to "heartbeat" for clarity (the
                // delivery path is the HeartbeatEngine).
                //
                // task_params may be a plain string (treated as content
                // addressed to a `cron-<job_id>` page) OR a JSON object
                // with explicit `page_id` and `content` fields, in which
                // case we honour the LLM's routing intent.
                let default_page = format!("cron-{}", job.id);
                let (target_page, target_content) =
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&job.task_params) {
                        if let Some(obj) = val.as_object() {
                            let page = obj.get("page_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or(default_page.clone());
                            let content = obj.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| job.task_params.clone());
                            (page, content)
                        } else {
                            (default_page, job.task_params.clone())
                        }
                    } else {
                        (default_page, job.task_params.clone())
                    };
                let _ = heartbeat.push(job.target_channel_id, target_page, target_content).await;
            } else if job.task_type == "work_iq_digest" {
                if let Err(e) = work_iq.generate_and_send_digest(&job.task_params, &job.target_channel_id).await {
                    tracing::error!("Work IQ: failed to run digest: {}", e);
                }
            }
        });
        fut
    });

    let cron_scheduler = hydragent_scheduler::CronScheduler::new(store.pool().clone(), executor)
        .await
        .expect("Failed to initialize CronScheduler");

    // Add schedule and send tools to registry
    let cron_scheduler_clone = cron_scheduler.clone();
    let schedule_tool = hydragent_tools::schedule_task::ScheduleTaskTool::new(move |cron_expr, desc, task_type, task_params, channel_id| {
        let scheduler = cron_scheduler_clone.clone();
        Box::pin(async move {
            scheduler.add_job(&cron_expr, &desc, &task_type, &task_params, &channel_id).await
        })
    });

    let heartbeat_clone = heartbeat.clone();
    let send_message_tool = hydragent_tools::send_message::SendMessageTool::new(move |channel_id, page_id, content| {
        let heartbeat = heartbeat_clone.clone();
        Box::pin(async move {
            heartbeat.push(channel_id, page_id, content).await
        })
    });

    // Add rss_subscribe tool to registry
    let work_iq_subscribe = work_iq.clone();
    let cron_scheduler_subscribe = cron_scheduler.clone();
    let rss_subscribe_tool = hydragent_tools::rss_subscribe::RssSubscribeTool::new(move |url, name, keywords, digest_channel, digest_cron| {
        let work_iq = work_iq_subscribe.clone();
        let scheduler = cron_scheduler_subscribe.clone();
        Box::pin(async move {
            // Add feed to DB
            work_iq.add_feed(&url, &name, &keywords, &digest_channel, &digest_cron).await?;
            // Add cron job for feed digest
            let _ = scheduler.add_job(
                &digest_cron,
                &format!("Work IQ Digest for {}", name),
                "work_iq_digest",
                &url,
                &digest_channel,
            ).await?;
            Ok(())
        })
    });

    registry.register(schedule_tool);
    registry.register(send_message_tool);
    registry.register(rss_subscribe_tool);

    let registry = Arc::new(registry);
    if registry_cell.set(registry.clone()).is_err() {
        panic!("Failed to initialize ToolRegistry OnceCell");
    }

    // Spawn background Work IQ polling interval loop
    let work_iq_loop = work_iq.clone();
    tokio::spawn(async move {
        let poll_interval_sec = std::env::var("WORK_IQ_POLL_INTERVAL_SEC")
            .unwrap_or_else(|_| "300".to_string())
            .parse::<u64>()
            .unwrap_or(300);
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(poll_interval_sec));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            tracing::info!("Starting background Work IQ feed polling cycle...");
            if let Err(e) = work_iq_loop.run_poll_cycle().await {
                tracing::error!("Error in background Work IQ polling cycle: {}", e);
            }
        }
    });

    // Initialize ActivePermissions
    let active_permissions = orchestrator::ActivePermissions::default();

    // Initialize pending-clarification map (one pending question per page).
    let pending_clarifications = std::sync::Arc::new(tokio::sync::Mutex::new(
        std::collections::HashMap::<String, hydragent_types::PendingClarification>::new(),
    ));

    // Initialize Event Bus Router and register handlers
    let mut router = hydragent_bus::router::Router::new();
    router.register("intent.submit", orchestrator::IntentSubmitHandler {
        store: store.clone(),
        model_router: model_router.clone(),
        registry,
        max_react_steps: app_config.max_react_steps,
        active_permissions: active_permissions.clone(),
        gateway_router: gateway_router.clone(),
        pending_clarifications: pending_clarifications.clone(),
    });
    router.register("permission.respond", orchestrator::PermissionRespondHandler {
        active_permissions,
    });
    router.register("gateway.register", orchestrator::GatewayRegisterHandler {
        gateway_router: gateway_router.clone(),
    });
    router.register("memory.list", orchestrator::MemoryListHandler {
        store: store.clone(),
    });
    router.register("memory.delete", orchestrator::MemoryDeleteHandler {
        store: store.clone(),
    });
    router.register("memory.clear", orchestrator::MemoryClearHandler {
        store: store.clone(),
    });
    router.register("memory.search", orchestrator::MemorySearchHandler {
        store: store.clone(),
    });
    router.register("dream.run", orchestrator::DreamRunHandler {
        store: store.clone(),
        model_router: model_router.clone(),
    });
    router.register("library.create_node", orchestrator::LibraryNodeCreateHandler {
        store: store.clone(),
    });
    router.register("library.link", orchestrator::LibraryLinkHandler {
        store: store.clone(),
    });
    router.register("library.search", orchestrator::LibrarySearchHandler {
        store: store.clone(),
    });
    router.register("library.list_nodes", orchestrator::LibraryNodeListHandler {
        store: store.clone(),
    });
    router.register("library.delete_node", orchestrator::LibraryNodeDeleteHandler {
        store: store.clone(),
    });
    router.register("page.compact", orchestrator::PageCompactHandler {
        store: store.clone(),
        model_router: model_router.clone(),
    });
    router.register("page.get_summary", orchestrator::PageGetSummaryHandler {
        store: store.clone(),
    });
    router.register("page.update_summary", orchestrator::PageUpdateSummaryHandler {
        store: store.clone(),
    });
    router.register("config.read", orchestrator::ConfigReadHandler);
    router.register("config.write", orchestrator::ConfigWriteHandler);

    // Create and start the Event Bus
    let bus = hydragent_bus::EventBus::new(router, app_config.bus_port);

    info!("Starting Event Bus server on port {}...", app_config.bus_port);
    if let Err(e) = bus.start().await {
        error!("Event Bus server failed to run: {}", e);
        std::process::exit(1);
    }
}
