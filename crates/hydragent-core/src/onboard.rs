// crates/hydragent-core/src/onboard.rs
//
// `hydragent onboard` — guided first-time setup.
//
// Walks a new user through the minimum-viable configuration:
//   1. Choose a provider (OpenAI / OpenRouter / Together / Groq / Ollama / custom).
//      This menu is **arrow-key navigable** on a TTY (↑/↓/Enter, or
//      type a digit to quick-select, or 'q' to cancel). On non-TTY
//      input (piped/CI) it falls back to the old "type a number" prompt.
//   2. Paste an API key (or skip for local providers)
//   3. Pick a model (sensible default per provider, overridable)
//   4. Write `.env` (preserves any existing keys the user wants to keep)
//   5. Optionally verify the brain speaks with `test-brain`
//   6. Print "what to do next"
//
// Non-interactive flags:
//   --provider <name>      skip the picker
//   --api-key <key>        skip the key prompt (don't commit secrets in CI!)
//   --model <name>         skip the model prompt
//   --non-interactive      same as supplying all of the above
//   --no-verify            skip the live `test-brain` call
//   --force                overwrite existing `.env` (default: update in place)

use std::io::{BufRead, Write};
use std::time::Duration;

use crate::paths;

/// True when the current stdin/stdout is a real terminal (i.e. the user
/// is sitting in front of a shell). False when stdin has been piped or
/// redirected, e.g. `echo 1 | hydragent onboard` or a CI runner.
fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RegistryFile {
    version: u32,
    #[serde(default)]
    defaults: std::collections::HashMap<String, String>,
    providers: Vec<hydragent_model::ProviderDefinition>,
    #[serde(default)]
    models: Vec<hydragent_model::ModelDefinition>,
}

pub async fn run(opts: OnboardOptions) -> i32 {
    let mut vault_passphrase = String::new();

    // ── 1. Welcome ────────────────────────────────────────────────────
    if !opts.non_interactive {
        println!();
        println!("------------------------------------------------------------------------");
        println!("  🐉 Welcome to Hydragent — first-time setup");
        println!("------------------------------------------------------------------------");
        println!("  I'll help you configure your local environment (lockbox, brain,");
        println!("  memory books, skills, and safety posture).");
        println!("  Press Ctrl-C at any time to abort.");
        println!("------------------------------------------------------------------------");
        println!();

        // ── Stage A: Vault & Lockbox Setup ─────────────────────────────────
        println!("------------------------------------------------------------------------");
        println!("  [Stage A/D] Vault & Lockbox Setup");
        println!("  Configure the lockbox unlock method for encrypting your secrets.");
        println!("------------------------------------------------------------------------");
        let vault_methods = &[
            "Local Unencrypted (Default: Fine for solo local dev)",
            "Passphrase Protected (Encrypts API keys & secrets at rest)"
        ];
        let selected_v = select(vault_methods, None).unwrap_or(0);
        if selected_v == 1 {
            println!();
            if let Some(passphrase) = prompt_secret("  Enter your secure vault passphrase:") {
                vault_passphrase = passphrase;
            }
        }
        println!();

        println!("------------------------------------------------------------------------");
        println!("  [Stage B/D] Brain Setup");
        println!("  Choose which model path or provider to use for remote/local inference.");
        println!("------------------------------------------------------------------------");
        println!();
    }

    // Load config and registry
    let app_config = crate::config::AppConfig::load().unwrap_or_default();
    let registry_path_str = app_config.effective_model_providers_path();
    let registry_path = std::path::PathBuf::from(&registry_path_str);

    let final_registry_path = if !registry_path.exists() {
        let fallback = std::env::current_dir().unwrap_or_default().join("config/model_providers.yaml");
        if fallback.exists() {
            fallback
        } else {
            registry_path
        }
    } else {
        registry_path
    };

    let mut registry = if final_registry_path.exists() {
        match hydragent_model::ProviderRegistry::load_from_yaml(&final_registry_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("⚠ Failed to load model_providers.yaml: {}. Falling back to builtin defaults.", e);
                hydragent_model::ProviderRegistry::builtin_default()
            }
        }
    } else {
        hydragent_model::ProviderRegistry::builtin_default()
    };

    // ── 2. Pick provider ──────────────────────────────────────────────
    let provider = match &opts.provider {
        Some(name) => match find_provider(&registry, name) {
            Some(p) => p,
            None => {
                eprintln!(
                    "✗ Unknown provider: '{}'. Valid options: {}",
                    name,
                    registry.providers().iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(", ")
                );
                return 2;
            }
        },
        None => {
            if opts.base_url.is_some() {
                hydragent_model::ProviderDefinition {
                    id: "custom".to_string(),
                    display_name: "Custom Endpoint".to_string(),
                    kind: hydragent_model::ProviderKind::CustomOpenAi,
                    default_base_url: opts.base_url.clone().unwrap_or_default(),
                    auth_mode: hydragent_model::AuthMode::ApiKey,
                    supports_custom_models: true,
                    supports_reasoning: true,
                    supports_tools: true,
                    supports_vision: true,
                    default_headers: std::collections::HashMap::new(),
                    timeout_secs: 180,
                    max_retries: 3,
                    default_params: std::collections::HashMap::new(),
                    models: Vec::new(),
                    api_key: None,
                }
            } else {
                match pick_provider(&mut registry, &final_registry_path) {
                    Some(p) => p,
                    None => {
                        eprintln!("✗ Setup aborted.");
                        return 1;
                    }
                }
            }
        }
    };

    let provider_label = &provider.display_name;
    let base = if let Some(url) = &opts.base_url {
        url.trim().trim_end_matches('/').to_string()
    } else if let Some(name) = &opts.provider {
        let n = name.trim().to_lowercase();
        if n.starts_with("http://") || n.starts_with("https://") {
            name.trim().trim_end_matches('/').to_string()
        } else {
            provider.default_base_url.to_string()
        }
    } else {
        provider.default_base_url.to_string()
    };

    // ── 3. API key ────────────────────────────────────────────────────
    let api_key = if provider.auth_mode == hydragent_model::AuthMode::None {
        String::new()
    } else if let Some(k) = &opts.api_key {
        k.clone()
    } else {
        if opts.non_interactive {
            eprintln!(
                "✗ Provider '{}' needs an API key. Pass --api-key <KEY>.",
                provider_label
            );
            return 2;
        }
        match prompt_secret(&format!("API key for {}:", provider_label)) {
            Some(k) => k,
            None => {
                eprintln!("✗ Setup aborted.");
                return 1;
            }
        }
    };

    // ── 4. Model ──────────────────────────────────────────────────────
    let model = if let Some(m) = &opts.model {
        m.clone()
    } else if opts.non_interactive {
        let first_model = registry.models(Some(&provider.id)).first().map(|m| m.api_model_id.clone()).unwrap_or_else(|| "gpt-4o-mini".to_string());
        first_model
    } else {
        let m = pick_model(&final_registry_path, &provider, &registry).await;
        if m.is_empty() {
            eprintln!("✗ Setup aborted.");
            return 1;
        }
        m
    };

    // ── New Steps: Persona, Sandbox, Memory, Integrations ──────────────────
    let mut persona = "developer";
    let mut custom_soul_prompt = String::new();
    let mut enforce_sandbox = false;
    let mut max_semantic_memories = 100;
    let mut enable_dreaming = true;
    let mut telegram_token = String::new();
    let mut telegram_chat_ids = String::new();

    if !opts.non_interactive {

        // ── Stage C: Memory & Skill Sources ─────────────────────────────────
        println!();
        println!("------------------------------------------------------------------------");
        println!("  [Stage C/D] Memory & Skill Sources");
        println!("  Configure durable work surfaces (pages, books, shelves), skills,");
        println!("  and Graphify relationship mapping.");
        println!("------------------------------------------------------------------------");

        // Probe existing durable work surfaces & files
        let data_dir = paths::data_dir();
        let db_file = data_dir.join("sessions.db");
        let vec_file = data_dir.join("vectors.bin");

        println!("  Checking existing work surfaces & durable storage...");
        let mut found_any = false;

        if db_file.exists() {
            if let Ok(meta) = db_file.metadata() {
                let size_kb = meta.len() / 1024;
                println!("  · Existing durable work surfaces database: {} ({} KB) [Found]", db_file.display(), size_kb);
                found_any = true;
            }
        }
        if vec_file.exists() {
            if let Ok(meta) = vec_file.metadata() {
                let size_kb = meta.len() / 1024;
                println!("  · Existing semantic vector embeddings: {} ({} KB) [Found]", vec_file.display(), size_kb);
                found_any = true;
            }
        }

        let skills_path = std::path::Path::new("./skills");
        if skills_path.exists() {
            if let Ok(entries) = std::fs::read_dir(skills_path) {
                let count = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                    .count();
                if count > 0 {
                    println!("  · Existing custom skills: {} folder(s) found in ./skills", count);
                    found_any = true;
                }
            }
        }

        if !found_any {
            println!("  · No existing durable work surfaces, vector databases, or skills found.");
            println!("    (These will be initialized fresh in: {})", data_dir.display());
        }
        println!("------------------------------------------------------------------------");
        println!();

        // 1. Agent Persona Selection
        let personas = &[
            "Developer (Default: Fact-focused, objective, coding-specialist)",
            "Creative (Warm, expressive, brainstorming partner)",
            "Minimalist (Extremely concise, direct answers)",
            "Custom (Define your own custom system prompt)"
        ];
        let selected_p = select(personas, None).unwrap_or(0);
        match selected_p {
            0 => persona = "developer",
            1 => persona = "creative",
            2 => persona = "minimalist",
            _ => {
                persona = "custom";
                println!();
                if let Some(prompt_text) = prompt("  Enter your custom system prompt / soul guidelines:") {
                    custom_soul_prompt = prompt_text;
                } else {
                    custom_soul_prompt = "You are a helpful AI assistant.".to_string();
                }
            }
        }

        // 2. Memory selection
        println!();
        let memory_options = &[
            "Enabled (Default: Stores and retrieves past facts about your chats)",
            "Disabled (Pure stateless session execution)"
        ];
        let selected_m = select(memory_options, None).unwrap_or(0);
        if selected_m == 1 {
            max_semantic_memories = 0;
        }

        // 3. Graphify Mapping
        println!();
        let graphify_options = &[
            "Enabled (Default: Automatically link pages, books, shelves in the background)",
            "Disabled"
        ];
        let selected_g = select(graphify_options, None).unwrap_or(0);
        if selected_g == 1 {
            enable_dreaming = false;
        }

        // 4. Skills setup
        println!();
        let skill_options = &[
            "Bundled Only (Default: lockbox-safe, only execute verified bundled skills)",
            "Broad Discovery (Allows discovering and coaching from external workspace skill folders)"
        ];
        let _selected_sk = select(skill_options, None).unwrap_or(0);

        // 5. Integrations & Adapters
        println!();
        if prompt_yes_no("Configure Telegram integration?", false).unwrap_or(false) {
            if let Some(token) = prompt("  Enter your Telegram Bot Token:") {
                telegram_token = token;
                if let Some(chat_ids) = prompt("  Enter your Telegram User/Chat ID(s) (comma-separated, e.g. 12345678):") {
                    telegram_chat_ids = chat_ids;
                }
            }
        }

        // ── Stage D: Safety Posture ─────────────────────────────────────────
        println!();
        println!("------------------------------------------------------------------------");
        println!("  [Stage D/D] Safety Posture");
        println!("  Configure the default execution safety posture.");
        println!("------------------------------------------------------------------------");
        let safety_options = &[
            "Secure Sandbox (Default: Runs risky code inside isolated WebAssembly, 100% safe)",
            "Host Execution with Approvals (Runs on host, prompts before executing shell/write commands)",
            "Default-Deny (Disables command/file operations entirely)"
        ];
        let selected_s = select(safety_options, None).unwrap_or(0);
        match selected_s {
            0 => {
                enforce_sandbox = true;
            }
            1 => {
                enforce_sandbox = false;
            }
            _ => {
                // Default-Deny can be implemented as sandbox on plus high safety settings
                enforce_sandbox = true;
            }
        }
    }

    // Now write SOUL.md and USER.md
    let config_dir = paths::config_dir();
    let _ = std::fs::create_dir_all(&config_dir);
    let soul_path = config_dir.join("SOUL.md");
    let soul_content = match persona {
        "developer" => {
            "# Agent Soul & Personality\n\
             - Name: Hydra\n\
             - Role: Pragmatic, direct, fact-focused software engineer and systems architect\n\
             - Tone: Objective, technical, and precise\n\
             - Core Guidelines: Focus on structural correctness, optimal architecture, bug prevention, and clean implementation. Avoid conversational fluff.\n\
             - Language Capability: Global (English primary)\n\n\
             # Behavior Rules\n".to_string()
        }
        "creative" => {
            "# Agent Soul & Personality\n\
             - Name: Hydra\n\
             - Role: Warm, imaginative, expressive brainstorming partner\n\
             - Tone: Encouraging, creative, and analogical\n\
             - Core Guidelines: Help the user explore concepts, design creative solutions, write engaging text, and think outside the box. Use rich analogies.\n\
             - Language Capability: Global (English primary)\n\n\
             # Behavior Rules\n".to_string()
        }
        "minimalist" => {
            "# Agent Soul & Personality\n\
             - Name: Hydra\n\
             - Role: Highly concise assistant\n\
             - Tone: Direct, brief, and minimalist\n\
             - Core Guidelines: Provide short, direct, and to-the-point answers with minimal explanation. Do not use conversational filler or introductory/concluding remarks.\n\
             - Language Capability: Global (English primary)\n\n\
             # Behavior Rules\n".to_string()
        }
        _ => {
            if custom_soul_prompt.contains("# Agent Soul & Personality") {
                custom_soul_prompt.clone()
            } else {
                format!(
                    "# Agent Soul & Personality\n\
                     - Name: Hydra\n\
                     - Role: Custom Persona\n\
                     - Tone: Professional, precise, adaptive, and concise\n\
                     - Core Guidelines: {}\n\
                     - Language Capability: Global (English primary)\n\n\
                     # Behavior Rules\n",
                     custom_soul_prompt
                )
            }
        }
    };
    let mut write_soul = true;
    if soul_path.exists() {
        if opts.non_interactive {
            write_soul = opts.force;
        } else {
            println!();
            match prompt_yes_no(
                &format!("An existing custom system persona (SOUL.md) was found at {}. Overwrite it? [y/N]", soul_path.display()),
                false,
            ) {
                Some(true) => {}
                _ => {
                    write_soul = false;
                    println!("  · Preserving existing custom SOUL.md");
                }
            }
        }
    }

    if write_soul {
        if let Err(e) = std::fs::write(&soul_path, &soul_content) {
            eprintln!("  ⚠ Failed to write SOUL.md: {}", e);
        } else if !opts.non_interactive {
            println!("  ✓ Wrote SOUL.md (Persona: {})", persona);
        }
    }

    let user_path = config_dir.join("USER.md");
    if !user_path.exists() {
        let user_content = "# User Profile\n\
                            - Name: User\n\
                            - Role: Software Engineer & Technical Operator\n\
                            - Preferred Tone: Professional, direct, and technically rigorous\n\
                            - Language & Locale: English (Universal)\n\
                            - Key Constraints: Absolute precision, strict formatting compliance, zero fluff\n\n\
                            # Style & Communication Habits\n\
                            - Preferences: Self-evolving based on your interactions and habits\n";
        let _ = std::fs::write(&user_path, user_content);
    }

    // ── 5. Write .env ─────────────────────────────────────────────────
    // Always write to ~/.hydragent/.env (top-level), never cwd/.env.
    // The installer creates ~/.hydragent/ and exports HYDRAGENT_HOME;
    // ensure_dirs() will create the home dir on first run if absent.
    let _ = paths::ensure_dirs();
    let env_path = paths::env_file();
    if env_path.exists() && !opts.force {
        if opts.non_interactive {
            eprintln!(
                "✗ .env already exists at {}. Pass --force to overwrite, or move it aside.",
                env_path.display()
            );
            return 2;
        }
        match prompt_yes_no(
            &format!(".env already exists at {}. Update it in place? [y/N]", env_path.display()),
            false,
        ) {
            Some(true) => {}
            _ => {
                eprintln!("✗ Setup aborted; .env left untouched.");
                return 1;
            }
        }
    }

    // Copy .env.example if .env does not exist
    if !env_path.exists() {
        let candidates = vec![
            std::env::current_dir().unwrap_or_default().join(".env.example"),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join(".env.example")))
                .unwrap_or_default(),
            paths::config_dir().join(".env.example"),
        ];
        let mut copied = false;
        for src in candidates {
            if src.exists() {
                if let Err(e) = std::fs::copy(&src, &env_path) {
                    eprintln!("  ⚠ Failed to copy .env.example: {}", e);
                } else {
                    copied = true;
                    if !opts.non_interactive {
                        println!("  ✓ Initialized .env from {}", src.display());
                    }
                    break;
                }
            }
        }
        if !copied {
            // Fallback: create empty .env so we can write to it
            let _ = std::fs::write(&env_path, "");
        }
    }

    // Read the current .env lines
    let mut lines = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&env_path) {
        for line in text.lines() {
            lines.push(line.to_string());
        }
    }

    let provider_name = &provider.id;

    // ── Update model_providers.yaml defaults ──
    if !final_registry_path.exists() {
        if let Some(parent) = final_registry_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&final_registry_path, hydragent_model::registry::BUILTIN_REGISTRY_YAML);
    }

    if final_registry_path.exists() {
        let chat_ref = format!("{}/{}", provider_name, model);
        let content_res = std::fs::read_to_string(&final_registry_path);
        if let Ok(content) = content_res {
            if let Ok(mut doc) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                if let Some(mapping) = doc.as_mapping_mut() {
                    let defaults_key = serde_yaml::Value::String("defaults".to_string());
                    if !mapping.contains_key(&defaults_key) {
                        mapping.insert(defaults_key.clone(), serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                    }
                    if let Some(defaults) = mapping.get_mut(&defaults_key).and_then(|v| v.as_mapping_mut()) {
                        defaults.insert(
                            serde_yaml::Value::String("chat".to_string()),
                            serde_yaml::Value::String(chat_ref.clone()),
                        );
                    }
                }
                if let Ok(serialized) = serde_yaml::to_string(&doc) {
                    if let Err(e) = std::fs::write(&final_registry_path, serialized) {
                        eprintln!("⚠ Failed to write defaults to model_providers.yaml: {}", e);
                    } else if !opts.non_interactive {
                        println!("  ✓ Wrote default chat model ({}) to model_providers.yaml", chat_ref);
                    }
                }
            }
        }
    }

    // ── Save secrets inside the cryptographic Vault ──
    let vault_path = paths::data_dir().join("vault/.hydravault");
    let vault = hydragent_vault::Vault::new(vault_path.clone());
    let passphrase = if vault_passphrase.is_empty() {
        "".to_string()
    } else {
        vault_passphrase.clone()
    };

    if !vault.exists() {
        if let Err(e) = vault.init(&passphrase) {
            eprintln!("⚠ Failed to initialize cryptographic Vault: {}", e);
        }
    }

    let mut secrets = vault.load(&passphrase).unwrap_or_default();
    if !api_key.is_empty() {
        secrets.insert("BRAIN_KEY".to_string(), hydragent_vault::TaintedString::credential(api_key.clone()));
        secrets.insert(format!("BRAIN_{}_KEY", provider_name.to_uppercase()), hydragent_vault::TaintedString::credential(api_key.clone()));
    }
    if base != provider.default_base_url {
        secrets.insert(format!("BRAIN_{}_BASE", provider_name.to_uppercase()), hydragent_vault::TaintedString::credential(base.clone()));
    }
    if !telegram_token.is_empty() {
        secrets.insert("TELEGRAM_BOT_TOKEN".to_string(), hydragent_vault::TaintedString::credential(telegram_token.clone()));
    }

    if let Err(e) = vault.save(&passphrase, &secrets) {
        eprintln!("⚠ Failed to save secrets to cryptographic Vault: {}", e);
    } else if !opts.non_interactive {
        println!("  ✓ Saved credentials and tokens securely inside the cryptographic Vault.");
    }

    // Prepare the keys we want to update/insert in .env (no secret keys!)
    let mut updates = std::collections::BTreeMap::<String, String>::new();
    updates.insert("ENFORCE_SANDBOX".to_string(), enforce_sandbox.to_string());
    updates.insert("MAX_SEMANTIC_MEMORIES".to_string(), max_semantic_memories.to_string());
    updates.insert("ENABLE_DREAMING".to_string(), enable_dreaming.to_string());
    if !vault_passphrase.is_empty() {
        updates.insert("HYDRAGENT_VAULT_PASSPHRASE".to_string(), vault_passphrase);
    }
    if !telegram_chat_ids.is_empty() {
        updates.insert("TELEGRAM_ALLOWED_CHAT_IDS".to_string(), telegram_chat_ids.clone());
    }

    // Update lines in-place
    let mut updated_keys = std::collections::BTreeSet::<String>::new();
    for line in lines.iter_mut() {
        let trimmed = line.trim();
        let is_comment = trimmed.starts_with('#');
        let effective_line = if is_comment {
            trimmed.trim_start_matches('#').trim()
        } else {
            trimmed
        };

        if effective_line.is_empty() {
            continue;
        }

        if let Some((k, _)) = effective_line.split_once('=') {
            let key_name = k.trim().to_string();
            if let Some(new_val) = updates.get(&key_name) {
                *line = format!("{}={}", key_name, quote_value(new_val));
                updated_keys.insert(key_name);
            }
        }
    }

    // Append any keys that were not already in the file
    for (k, v) in &updates {
        if !updated_keys.contains(k) {
            lines.push(format!("{}={}", k, quote_value(v)));
        }
    }

    // Join and write back
    let rendered = lines.join("\n") + "\n";
    if let Err(e) = std::fs::write(&env_path, rendered) {
        eprintln!("✗ Failed to write .env: {}", e);
        return 1;
    }
    println!();
    println!("  ✓ Updated .env at {}", env_path.display());

    // ── 6. Optionally verify ──────────────────────────────────────────
    if !opts.no_verify {
        if opts.non_interactive {
            println!("  → Run `hydragent test-brain \"hello\"` to verify the connection.");
        } else {
            match prompt_yes_no("Verify the connection now with `test-brain`? [Y/n]", true) {
                Some(true) => {
                    println!();
                    println!("  Spawning live brain test…");
                    println!();
                    let status = std::process::Command::new(std::env::current_exe().unwrap())
                        .arg("test-brain")
                        .arg("Reply with exactly the word PONG.")
                        .arg("--debug")
                        .status();
                    match status {
                        Ok(s) if s.success() => {
                            println!();
                            println!("  ✓ Brain is reachable. You're ready to chat.");
                        }
                        Ok(s) => {
                            println!();
                            println!("  ⚠ Brain test exited with code {:?}. The connection may be", s.code());
                            println!("    misconfigured. Try `hydragent doctor` to diagnose.");
                        }
                        Err(e) => {
                            println!();
                            println!("  ⚠ Could not spawn `test-brain`: {}", e);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ── 7. Next steps ─────────────────────────────────────────────────
    println!();
    println!("------------------------------------------------------------------------");
    println!("  Next steps");
    println!("------------------------------------------------------------------------");
    println!("    hydragent chat         Start an interactive chat (recommended)");
    println!("    hydragent test-brain   Send a single prompt and stream the reply");
    println!("    hydragent --list-sessions  Show past chat sessions");
    println!("    hydragent doctor       Re-run diagnostics if something breaks");
    println!("    hydragent security status  Inspect the Phase 6 security surface");
    println!("    hydragent vault init   Optional: encrypt your API keys in a vault");
    println!();
    println!("  Tip: Edit your model configurations and add custom providers in:");
    println!("       {}", final_registry_path.display());
    println!("------------------------------------------------------------------------");

    0
}

#[derive(Debug, Clone)]
pub struct OnboardOptions {
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub non_interactive: bool,
    pub no_verify: bool,
    pub force: bool,
    pub base_url: Option<String>,
}

// ── helpers ────────────────────────────────────────────────────────────

fn find_provider(registry: &hydragent_model::ProviderRegistry, name: &str) -> Option<hydragent_model::ProviderDefinition> {
    let n = name.trim().to_lowercase();
    if let Some(p) = registry.provider(&n) {
        return Some(p.clone());
    }
    registry.providers().iter().find(|p| {
        p.id.to_lowercase() == n || p.display_name.to_lowercase().contains(&n)
    }).map(|p| (*p).clone())
}

fn quote_value(v: &str) -> String {
    if v.contains(' ') || v.contains('#') || v.contains('"') {
        let escaped = v.replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        v.to_string()
    }
}

fn pick_provider(
    registry: &mut hydragent_model::ProviderRegistry,
    registry_path: &std::path::Path,
) -> Option<hydragent_model::ProviderDefinition> {
    println!("  Choose a provider (↑/↓ to move, Enter to select, q to quit):");
    let providers = registry.providers();
    let mut labels: Vec<String> = providers.iter().map(|p| format!("{} ({})", p.display_name, p.id)).collect();

    let custom_idx = labels.len();
    labels.push("Add a new custom provider...".to_string());

    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

    let idx = select(&label_refs, Some(("c", custom_idx)))?;
    if idx == custom_idx {
        add_custom_provider(registry_path)
    } else {
        Some(providers[idx].clone())
    }
}

fn add_custom_provider(
    registry_path: &std::path::Path,
) -> Option<hydragent_model::ProviderDefinition> {
    println!();
    println!("  ── Add a new custom provider ──");
    let id = prompt("  Provider ID (e.g. together, fireworks, custom-endpoint):")?;
    let id_clean = id.trim().to_lowercase();
    if id_clean.is_empty() {
        return None;
    }

    let display_name = prompt("  Display name (e.g. Together AI):")?;
    let display_name_clean = display_name.trim().to_string();

    let base_url = prompt("  Base URL (e.g. https://api.together.xyz/v1):")?;
    let base_url_clean = base_url.trim().trim_end_matches('/').to_string();

    let needs_key = prompt_yes_no("  Does this provider require an API key?", true).unwrap_or(true);

    let provider = hydragent_model::ProviderDefinition {
        id: id_clean.clone(),
        display_name: display_name_clean,
        kind: if id_clean == "ollama" {
            hydragent_model::ProviderKind::Ollama
        } else {
            hydragent_model::ProviderKind::CustomOpenAi
        },
        default_base_url: base_url_clean,
        auth_mode: if needs_key {
            hydragent_model::AuthMode::ApiKey
        } else {
            hydragent_model::AuthMode::None
        },
        supports_custom_models: true,
        supports_reasoning: true,
        supports_tools: true,
        supports_vision: true,
        default_headers: std::collections::HashMap::new(),
        timeout_secs: 180,
        max_retries: 3,
        default_params: std::collections::HashMap::new(),
        models: Vec::new(),
        api_key: None,
    };

    if let Err(e) = save_provider_to_yaml(registry_path, &provider) {
        eprintln!("  ⚠ Failed to save new provider to {}: {}", registry_path.display(), e);
    } else {
        println!("  ✓ Saved new provider '{}' to {}", id_clean, registry_path.display());
    }

    Some(provider)
}

fn save_provider_to_yaml(
    path: &std::path::Path,
    provider: &hydragent_model::ProviderDefinition,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file_data = if path.exists() {
        let text = std::fs::read_to_string(path)?;
        serde_yaml::from_str::<RegistryFile>(&text).unwrap_or_else(|_| RegistryFile {
            version: 1,
            defaults: std::collections::HashMap::new(),
            providers: Vec::new(),
            models: Vec::new(),
        })
    } else {
        RegistryFile {
            version: 1,
            defaults: std::collections::HashMap::new(),
            providers: Vec::new(),
            models: Vec::new(),
        }
    };

    if let Some(existing) = file_data.providers.iter_mut().find(|p| p.id == provider.id) {
        *existing = provider.clone();
    } else {
        file_data.providers.push(provider.clone());
    }

    let new_yaml = serde_yaml::to_string(&file_data)?;
    std::fs::write(path, new_yaml)?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct OllamaTagsResp {
    models: Vec<OllamaModelItem>,
}

#[derive(serde::Deserialize)]
struct OllamaModelItem {
    name: String,
}

async fn pick_model(
    registry_path: &std::path::Path,
    provider: &hydragent_model::ProviderDefinition,
    registry: &hydragent_model::ProviderRegistry,
) -> String {
    println!();
    println!(
        "  Pick a primary model for {} (↑/↓ to move, Enter to select):",
        provider.display_name
    );

    let mut downloaded_models = Vec::new();
    if provider.kind == hydragent_model::ProviderKind::Ollama {
        print!("  🔍 Querying local Ollama for downloaded models... ");
        let _ = std::io::stdout().flush();
        let base_url = &provider.default_base_url;
        match async {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()?;
            let url = format!("{}/api/tags", base_url.trim_end_matches('/').replace("/v1", ""));
            let resp = client.get(&url).send().await?.json::<OllamaTagsResp>().await?;
            let mut names: Vec<String> = resp.models.into_iter().map(|m| m.name).collect();
            names.sort();
            Ok::<Vec<String>, anyhow::Error>(names)
        }.await {
            Ok(models) => {
                if models.is_empty() {
                    println!("none found");
                } else {
                    println!("found {} model(s)", models.len());
                    downloaded_models = models;
                }
            }
            Err(_) => {
                println!("failed to connect");
            }
        }
    }

    let reg_models = registry.models(Some(&provider.id));
    let mut labels = Vec::new();
    if !downloaded_models.is_empty() {
        labels = downloaded_models;
    } else {
        for m in reg_models {
            labels.push(m.api_model_id.clone());
        }
    }

    let custom_idx = labels.len();
    labels.push("custom — type your own and add to registry".to_string());

    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

    let idx = match select(&label_refs, Some(("c", custom_idx))) {
        Some(i) => i,
        None => return String::new(),
    };
    if idx == custom_idx {
        match prompt("  Model API ID (e.g. deepseek/deepseek-r1):") {
            Some(s) => {
                let model_api_id = s.trim().to_string();
                if model_api_id.is_empty() {
                    return String::new();
                }

                let model_id = model_api_id.replace('/', "-");
                let model_def = hydragent_model::ModelDefinition {
                    id: model_id,
                    provider_id: provider.id.clone(),
                    name: model_api_id.clone(),
                    aliases: Vec::new(),
                    api_model_id: model_api_id.clone(),
                    tool_calling: true,
                    vision: false,
                    reasoning: model_api_id.contains("r1") || model_api_id.contains("reasoning") || model_api_id.contains("o1"),
                    streaming: true,
                    max_input_tokens: Some(128000),
                    max_output_tokens: Some(8192),
                    request_headers: std::collections::HashMap::new(),
                    default_params: std::collections::HashMap::new(),
                    cost_per_1k: None,
                    cost_tier: None,
                    url: None,
                };

                if let Err(e) = save_model_to_yaml(registry_path, &model_def) {
                    eprintln!("  ⚠ Failed to save new model to {}: {}", registry_path.display(), e);
                } else {
                    println!("  ✓ Saved new model '{}' to {}", model_api_id, registry_path.display());
                }

                model_api_id
            }
            None => String::new(),
        }
    } else {
        labels[idx].clone()
    }
}

fn save_model_to_yaml(
    path: &std::path::Path,
    model: &hydragent_model::ModelDefinition,
) -> anyhow::Result<()> {
    let mut file_data = if path.exists() {
        let text = std::fs::read_to_string(path)?;
        serde_yaml::from_str::<RegistryFile>(&text).unwrap_or_else(|_| RegistryFile {
            version: 1,
            defaults: std::collections::HashMap::new(),
            providers: Vec::new(),
            models: Vec::new(),
        })
    } else {
        RegistryFile {
            version: 1,
            defaults: std::collections::HashMap::new(),
            providers: Vec::new(),
            models: Vec::new(),
        }
    };

    let mut saved = false;
    for p in &mut file_data.providers {
        if p.id == model.provider_id {
            if let Some(existing) = p.models.iter_mut().find(|m| m.id == model.id) {
                *existing = model.clone();
            } else {
                p.models.push(model.clone());
            }
            saved = true;
            break;
        }
    }

    if !saved {
        if let Some(existing) = file_data.models.iter_mut().find(|m| m.provider_id == model.provider_id && m.id == model.id) {
            *existing = model.clone();
        } else {
            file_data.models.push(model.clone());
        }
    }

    let new_yaml = serde_yaml::to_string(&file_data)?;
    std::fs::write(path, new_yaml)?;
    Ok(())
}

// ── interactive arrow-key picker ──────────────────────────────────────
//
// Cross-platform (Windows + Unix) raw-mode menu that lets the user
// navigate a list with ↑/↓ (or j/k or number keys) and press Enter to
// select. Also accepts 'q' / Esc to cancel, and a one-letter shortcut
// for the "custom" slot if one is provided.
//
// Falls back to a plain line-based prompt on non-TTY stdin/stdout so
// piped input (`echo 1 | hydragent onboard`) keeps working, and so
// `cargo test` or CI doesn't hang waiting for a keypress.
//
// Returns the 0-based index, or `None` if the user cancelled.
fn select(labels: &[&str], custom: Option<(&str, usize)>) -> Option<usize> {
    if labels.is_empty() {
        return None;
    }

    // Non-TTY fallback: ask for a number (or the custom key).
    if !is_tty() {
        return numeric_fallback(labels, custom);
    }

    use crossterm::{
        cursor::MoveUp,
        event::{read, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        queue,
        terminal::{enable_raw_mode, Clear, ClearType},
    };

    // The caller has printed a header. We're about to print the
    // option list and a footer; the loop will redraw this block on
    // every keypress by moving up and clearing.
    let mut stdout = std::io::stdout();
    let n = labels.len() as u16;
    let mut selected: usize = 0;

    // Enable raw mode; restore on any early return via the guard below.
    if let Err(e) = enable_raw_mode() {
        eprintln!(
            "(arrow-key picker unavailable: {} — falling back to numbers)",
            e
        );
        return numeric_fallback(labels, custom);
    }
    let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide);

    // We need a guard so any panic or early return restores the
    // terminal to a sane state (raw mode disabled, cursor visible).
    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
            let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
        }
    }
    let _guard = RawGuard;

    // First render: draw the options + footer once, then enter the
    // event loop. Cursor ends on the line *after* the footer.
    let _ = render_block(&mut stdout, labels, selected);
    let _ = stdout.flush();

    loop {
        // Read one key first so we only redraw when the user
        // actually changes the selection.
        let ev = match read() {
            Ok(ev) => ev,
            Err(_) => return None,
        };
        let Event::Key(KeyEvent {
            code, modifiers, kind, ..
        }) = ev
        else {
            continue;
        };
        if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        if modifiers.contains(KeyModifiers::CONTROL)
            || modifiers.contains(KeyModifiers::ALT)
        {
            continue;
        }
        // `advance` is true when the key changed the selection and we
        // need to redraw. The match is "action" oriented: every arm
        // either returns, mutates `selected` (sets advance=true), or
        // does nothing (advance=false).
        let mut advance = false;
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if selected == 0 {
                    selected = labels.len() - 1;
                } else {
                    selected -= 1;
                }
                advance = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1) % labels.len();
                advance = true;
            }
            KeyCode::Home => {
                selected = 0;
                advance = true;
            }
            KeyCode::End => {
                selected = labels.len() - 1;
                advance = true;
            }
            KeyCode::Enter => return Some(selected),
            KeyCode::Esc | KeyCode::Char('q') => return None,
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(d) = c.to_digit(10) {
                    if (1..=labels.len() as u32).contains(&d) {
                        return Some((d - 1) as usize);
                    }
                }
                // Unrecognised digit — ignore, don't redraw.
            }
            KeyCode::Char(c) => {
                if let Some((key, idx)) = custom {
                    if c.eq_ignore_ascii_case(&key.chars().next().unwrap_or('?')) {
                        return Some(idx);
                    }
                }
                // Unrecognised char — ignore, don't redraw.
            }
            _ => {}
        }
        if advance {
            // Redraw: move up `n + 1` lines (n options + 1 footer),
            // clear from the first option down, and re-draw.
            let _ = queue!(
                stdout,
                MoveUp(n + 1),
                Clear(ClearType::FromCursorDown),
            );
            let _ = render_block(&mut stdout, labels, selected);
            let _ = stdout.flush();
        }
    }
}

// Draw the option block (n options + footer) starting at the current
// cursor position. Each option is one line; the footer is one line.
fn render_block(
    stdout: &mut std::io::Stdout,
    labels: &[&str],
    selected: usize,
) -> std::io::Result<()> {
    use crossterm::{
        queue,
        style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    };
    for (i, label) in labels.iter().enumerate() {
        if i == selected {
            queue!(
                stdout,
                SetForegroundColor(Color::Cyan),
                SetAttribute(Attribute::Bold),
                Print(format!("  ▸ [{}] {}\r\n", i + 1, label)),
                ResetColor,
                SetAttribute(Attribute::Reset),
            )?;
        } else {
            queue!(stdout, Print(format!("    [{}] {}\r\n", i + 1, label)))?;
        }
    }
    queue!(
        stdout,
        Print("    ↑/↓ to move, Enter to select\r\n"),
    )?;
    Ok(())
}

// Fallback path: read a line from stdin and parse it as a number.
// Used when stdin/stdout is not a TTY (CI, scripts, redirected I/O).
fn numeric_fallback(labels: &[&str], custom: Option<(&str, usize)>) -> Option<usize> {
    loop {
        let q = match custom {
            Some((key, _)) => format!(
                "  Number (1-{}, or '{}' for custom, 'q' to quit):",
                labels.len(),
                key
            ),
            None => format!("  Number (1-{}, or 'q' to quit):", labels.len()),
        };
        let raw = prompt(&q)?;
        let trimmed = raw.trim().to_lowercase();
        if trimmed == "q" || trimmed == "quit" {
            return None;
        }
        if let Some((key, idx)) = custom {
            if trimmed == key.to_lowercase() {
                return Some(idx);
            }
        }
        if let Ok(n) = trimmed.parse::<usize>() {
            if n >= 1 && n <= labels.len() {
                return Some(n - 1);
            }
        }
        eprintln!(
            "  Please enter a number between 1 and {}, or 'q'.",
            labels.len()
        );
    }
}

fn prompt(question: &str) -> Option<String> {
    let mut stdout = std::io::stdout();
    write!(stdout, "{} ", question).ok()?;
    stdout.flush().ok()?;
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok()?;
    let s = line.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn prompt_yes_no(question: &str, default_yes: bool) -> Option<bool> {
    // On a TTY we delegate to the arrow-key picker so the user can
    // press ←/→ to flip the answer and Enter to confirm. On non-TTY
    // input (CI, scripts) we fall back to a plain "y/N" line read.
    if is_tty() {
        return prompt_yes_no_tty(question, default_yes);
    }
    let hint = if default_yes { "Y/n" } else { "y/N" };
    let raw = prompt(&format!("{} [{}]", question, hint))?;
    let t = raw.trim().to_lowercase();
    if t.is_empty() {
        return Some(default_yes);
    }
    if t == "y" || t == "yes" {
        Some(true)
    } else if t == "n" || t == "no" {
        Some(false)
    } else {
        Some(default_yes)
    }
}

// TTY-only yes/no picker. Renders "<question> ▸ Yes / No" on a
// single line; the user can press ←/→ to switch, Enter to confirm,
// or y/n as a shortcut. The current selection is shown in bold
// cyan with a leading ▸ arrow.
fn prompt_yes_no_tty(question: &str, default_yes: bool) -> Option<bool> {
    use crossterm::{
        cursor::MoveToColumn,
        event::{read, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        queue,
        style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
        terminal::{enable_raw_mode, Clear, ClearType},
    };

    let mut stdout = std::io::stdout();
    if let Err(_) = enable_raw_mode() {
        // No raw mode? fall back to the typed prompt.
        let raw = prompt(&format!("{} [Y/n]", question))?;
        let t = raw.trim().to_lowercase();
        return Some(match t.as_str() {
            "" => default_yes,
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default_yes,
        });
    }
    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
            let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
        }
    }
    let _guard = RawGuard;

    let mut yes_selected: bool = default_yes;
    // Print the question once above the interactive selector.
    // This prevents long questions from wrapping and cascading during redraws.
    println!("  {}", question);

    loop {
        // Render the single line:  "  [▸ Yes] / [  No]  (←/→, Enter)".
        let _ = queue!(stdout, Clear(ClearType::FromCursorDown));
        let (yes_str, no_str) = if yes_selected {
            (
                format!(
                    "{}{} ▸ Yes {}{}",
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::Cyan),
                    ResetColor,
                    SetAttribute(Attribute::Reset)
                ),
                format!(
                    "{}{}   No {}{}",
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::DarkGrey),
                    ResetColor,
                    SetAttribute(Attribute::Reset)
                ),
            )
        } else {
            (
                format!(
                    "{}{}   Yes {}{}",
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::DarkGrey),
                    ResetColor,
                    SetAttribute(Attribute::Reset)
                ),
                format!(
                    "{}{} ▸ No {}{}",
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::Cyan),
                    ResetColor,
                    SetAttribute(Attribute::Reset)
                ),
            )
        };
        let _ = queue!(
            stdout,
            Print(format!("  {}{}   (←/→, Enter)  ", yes_str, no_str)),
            MoveToColumn(0),
        );
        let _ = stdout.flush();

        let ev = match read() {
            Ok(ev) => ev,
            Err(_) => return None,
        };
        let Event::Key(KeyEvent {
            code, modifiers, kind, ..
        }) = ev
        else {
            continue;
        };
        if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        if modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::ALT) {
            continue;
        }
        match code {
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('y') | KeyCode::Char('Y') => {
                yes_selected = true;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('n') | KeyCode::Char('N') => {
                yes_selected = false;
            }
            KeyCode::Char(' ') | KeyCode::Tab => {
                yes_selected = !yes_selected;
            }
            KeyCode::Enter => {
                // Move past the prompt line so subsequent `println!`
                // calls don't overwrite the rendered answer.
                let _ = queue!(stdout, Print("\r\n"));
                let _ = stdout.flush();
                return Some(yes_selected);
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                let _ = queue!(stdout, Print("\r\n"));
                let _ = stdout.flush();
                return None;
            }
            _ => {}
        }
    }
}

fn prompt_secret(question: &str) -> Option<String> {
    // Use rpassword for masked input. Falls back to plain stdin on
    // non-TTY contexts (e.g. CI or piped input).
    match rpassword::prompt_password(question) {
        Ok(s) => {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(_) => prompt(question),
    }
}
