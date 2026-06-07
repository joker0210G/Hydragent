// crates/hydragent-core/src/main.rs
pub mod orchestrator;
pub mod react_loop;
pub mod session;
pub mod logger;
pub mod config;

use clap::Parser;
use std::sync::Arc;
use hydragent_memory::SessionStore;
use tracing::{info, error};

#[derive(Parser, Debug)]
#[command(name = "hydragent", author, version, about = "Hydragent AI Agent core runtime")]
struct Args {
    /// Resumes or starts a specific chat session by ID
    #[arg(short, long)]
    session: Option<String>,

    /// Lists all past conversation sessions stored in the SQLite database and exits
    #[arg(long)]
    list_sessions: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Load configuration
    let app_config = config::AppConfig::load().unwrap_or_else(|e| {
        eprintln!("Failed to load configuration: {}", e);
        std::process::exit(1);
    });

    // Initialize the logger using configured values
    logger::init_logger(&app_config.log_format, &app_config.log_level);

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

    info!("Hydragent starting up with config: {:?}", app_config);

    // Initialize Event Bus Router and register handlers
    let mut router = hydragent_bus::router::Router::new();
    router.register("intent.submit", orchestrator::IntentSubmitHandler {
        store: store.clone(),
        model_router: model_router.clone(),
    });


    // Create and start the Event Bus
    let bus = hydragent_bus::EventBus::new(router, app_config.bus_port);

    info!("Starting Event Bus server on port {}...", app_config.bus_port);
    if let Err(e) = bus.start().await {
        error!("Event Bus server failed to run: {}", e);
        std::process::exit(1);
    }
}
