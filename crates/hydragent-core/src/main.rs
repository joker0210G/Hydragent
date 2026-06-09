// crates/hydragent-core/src/main.rs
pub mod orchestrator;
pub mod react_loop;
pub mod session;
pub mod logger;
pub mod config;
pub mod dream;

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
    let app_config = config::AppConfig::load().unwrap_or_else(|e| {
        eprintln!("Failed to load configuration: {}", e);
        std::process::exit(1);
    });

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
    let store = Arc::new(SessionStore::new(&db_path).await.unwrap_or_else(|e| {
        error!("Failed to initialize session database: {}", e);
        std::process::exit(1);
    }));

    // Parse API keys list
    let keys: Vec<String> = app_config.openrouter_api_keys
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    for key in &keys {
        if key.contains("9b9c8f09436e") {
            tracing::warn!("⚠️ Warning: Default placeholder API key detected in configuration. Please replace it with a valid OpenRouter API key in your .env file.");
            break;
        }
    }

    // Parse fallback models list
    let fallbacks: Vec<String> = app_config.fallback_models
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Initialize Model Provider (OpenRouter) & Model Router
    let provider = Arc::new(hydragent_model::openrouter::OpenRouterClient::new(keys));
    let model_router = Arc::new(hydragent_model::router::ModelRouter::new(
        provider.clone(),
        app_config.primary_model.clone(),
        fallbacks,
    ));

    if args.list_sessions {
        println!("------------------------------------------------------------------------");
        println!("  🐉 Hydragent Session History");
        println!("  Database: {}", db_path);
        println!("------------------------------------------------------------------------");
        match store.list_sessions().await {
            Ok(sessions) => {
                if sessions.is_empty() {
                    println!("  No active sessions found.");
                } else {
                    println!("  {:<36} | {:<20} | {:<5}", "Session ID", "Last Active", "Turns");
                    println!("  ----------------------------------------------------------------------");
                    for (session_id, _, last_active, turn_count) in sessions {
                        let dt = chrono::DateTime::from_timestamp(last_active / 1000, 0)
                            .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                            .unwrap_or_else(|| "Unknown".to_string());
                        println!("  {:<36} | {:<20} | {:<5}", session_id, dt, turn_count);
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to read database session list: {}", e);
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
            Commands::Embed { .. } | Commands::Vault { .. } => unreachable!(),
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
                match dream::run_dream_cycle(store_clone.clone(), model_router_clone.clone()).await {
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
        registry.register(EchoTool);
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
        registry.register(FileReadTool::new(PathBuf::from(&workspace_dir)));
    }
    registry.register(MemoryStoreTool::new(store.clone()));
    registry.register(MemorySearchTool::new(store.clone()));
    registry.register(MemoryForgetTool::new(store.clone()));
    registry.register(hydragent_tools::standing_orders::StandingOrdersTool::new(PathBuf::from("./config")));
    let registry = Arc::new(registry);

    // Initialize ActivePermissions
    let active_permissions = orchestrator::ActivePermissions::default();

    // Initialize Gateway Router
    let gateway_router = Arc::new(hydragent_gateway::GatewayRouter::new());

    // Initialize Event Bus Router and register handlers
    let mut router = hydragent_bus::router::Router::new();
    router.register("intent.submit", orchestrator::IntentSubmitHandler {
        store: store.clone(),
        model_router: model_router.clone(),
        registry,
        max_react_steps: app_config.max_react_steps,
        active_permissions: active_permissions.clone(),
        gateway_router: gateway_router.clone(),
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

    // Create and start the Event Bus
    let bus = hydragent_bus::EventBus::new(router, app_config.bus_port);

    info!("Starting Event Bus server on port {}...", app_config.bus_port);
    if let Err(e) = bus.start().await {
        error!("Event Bus server failed to run: {}", e);
        std::process::exit(1);
    }
}
