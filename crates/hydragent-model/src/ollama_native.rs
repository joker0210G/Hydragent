use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::model_trait::ModelProvider;
use crate::openrouter::LLMRequest;

// ============================================================================
// Configuration
// ============================================================================

#[derive(Debug, Clone)]
pub struct OllamaNativeConfig {
    pub base_url: String,
    pub default_model: String,
    pub timeout: Duration,
    pub default_num_ctx: u32,
    pub keep_alive: Option<String>,
    pub num_thread: Option<u32>,
    pub temperature: Option<f32>,
    pub repeat_penalty: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub thinking_mode: ThinkingMode,
    pub auto_serve: bool,
    pub auto_discover: bool,
    pub structured_output: bool,
    pub native_tool_calling: bool,
    pub stream_buffer_ms: u64,
    pub context_length_override: Option<u32>,
    pub model_metadata_ttl_sec: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingMode {
    Auto,
    Enabled,
    Disabled,
    Low,
    Medium,
    High,
    Max,
}

impl Default for ThinkingMode {
    fn default() -> Self {
        ThinkingMode::Auto
    }
}

impl std::fmt::Display for ThinkingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThinkingMode::Auto => write!(f, "auto"),
            ThinkingMode::Enabled => write!(f, "true"),
            ThinkingMode::Disabled => write!(f, "false"),
            ThinkingMode::Low => write!(f, "low"),
            ThinkingMode::Medium => write!(f, "medium"),
            ThinkingMode::High => write!(f, "high"),
            ThinkingMode::Max => write!(f, "max"),
        }
    }
}

impl OllamaNativeConfig {
    pub fn from_env() -> Self {
        let base_url = std::env::var("OLLAMA_API_BASE")
            .unwrap_or_else(|_| "http://localhost:11434".to_string())
            .trim_end_matches('/')
            .replace("/v1", "");

        let default_model = std::env::var("OLLAMA_MODEL")
            .unwrap_or_else(|_| "llama3.1:8b".to_string());

        let timeout_secs = std::env::var("OLLAMA_API_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(300);

        let default_num_ctx = std::env::var("OLLAMA_NUM_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(8192);

        let keep_alive = std::env::var("OLLAMA_KEEP_ALIVE")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let num_thread = std::env::var("OLLAMA_NUM_THREAD")
            .ok()
            .and_then(|s| s.parse::<u32>().ok());

        let temperature = std::env::var("OLLAMA_TEMPERATURE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok());

        let repeat_penalty = std::env::var("OLLAMA_REPEAT_PENALTY")
            .ok()
            .and_then(|s| s.parse::<f32>().ok());

        let top_p = std::env::var("OLLAMA_TOP_P")
            .ok()
            .and_then(|s| s.parse::<f32>().ok());

        let top_k = std::env::var("OLLAMA_TOP_K")
            .ok()
            .and_then(|s| s.parse::<u32>().ok());

        let thinking_mode = std::env::var("OLLAMA_THINKING")
            .ok()
            .and_then(|s| match s.to_lowercase().as_str() {
                "auto" => Some(ThinkingMode::Auto),
                "true" | "on" | "enabled" => Some(ThinkingMode::Enabled),
                "false" | "off" | "disabled" => Some(ThinkingMode::Disabled),
                "low" => Some(ThinkingMode::Low),
                "medium" => Some(ThinkingMode::Medium),
                "high" => Some(ThinkingMode::High),
                "max" => Some(ThinkingMode::Max),
                _ => None,
            })
            .unwrap_or_default();

        let auto_serve = std::env::var("OLLAMA_AUTO_SERVE")
            .ok()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(true);

        let auto_discover = std::env::var("OLLAMA_AUTO_DISCOVER")
            .ok()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(true);

        let structured_output = std::env::var("OLLAMA_STRUCTURED_OUTPUT")
            .ok()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(true);

        let native_tool_calling = std::env::var("OLLAMA_NATIVE_TOOL_CALLING")
            .ok()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(true);

        let stream_buffer_ms = std::env::var("OLLAMA_STREAM_BUFFER_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(10);

        let context_length_override = std::env::var("OLLAMA_CONTEXT_LENGTH")
            .ok()
            .and_then(|s| s.parse::<u32>().ok());

        let model_metadata_ttl_sec = std::env::var("OLLAMA_METADATA_TTL_SEC")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(300);

        Self {
            base_url,
            default_model,
            timeout: Duration::from_secs(timeout_secs),
            default_num_ctx,
            keep_alive,
            num_thread,
            temperature,
            repeat_penalty,
            top_p,
            top_k,
            thinking_mode,
            auto_serve,
            auto_discover,
            structured_output,
            native_tool_calling,
            stream_buffer_ms,
            context_length_override,
            model_metadata_ttl_sec,
        }
    }
}

// ============================================================================
// OS-Specific Ollama Server Management
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OllamaServerStatus {
    Running,
    NotRunning,
    Starting,
    Failed(String),
}

pub async fn check_ollama_server(base_url: &str) -> OllamaServerStatus {
    let client = match Client::builder()
        .timeout(Duration::from_secs(3))
        .build() {
        Ok(c) => c,
        Err(_) => return OllamaServerStatus::Failed("Cannot build HTTP client".to_string()),
    };

    let url = format!("{}/api/tags", base_url);
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => OllamaServerStatus::Running,
        Ok(resp) => OllamaServerStatus::Failed(format!("HTTP {}", resp.status())),
        Err(e) => {
            if e.is_connect() || e.is_timeout() {
                OllamaServerStatus::NotRunning
            } else {
                OllamaServerStatus::Failed(e.to_string())
            }
        }
    }
}

pub async fn ensure_ollama_server(base_url: &str) -> Result<bool> {
    let status = check_ollama_server(base_url).await;
    match status {
        OllamaServerStatus::Running => {
            info!("Ollama server is already running at {}", base_url);
            return Ok(true);
        }
        OllamaServerStatus::Starting => {
            info!("Ollama server is starting...");
            // Wait for it to be ready
            for _ in 0..30 {
                sleep(Duration::from_secs(1)).await;
                if matches!(check_ollama_server(base_url).await, OllamaServerStatus::Running) {
                    return Ok(true);
                }
            }
            return Err(anyhow::anyhow!("Ollama server failed to start within 30 seconds"));
        }
        _ => {}
    }

    info!("Ollama server not running, attempting to start...");
    let started = start_ollama_server().await;

    if started {
        // Wait for server to be ready
        for _ in 0..60 {
            sleep(Duration::from_secs(1)).await;
            if matches!(check_ollama_server(base_url).await, OllamaServerStatus::Running) {
                info!("Ollama server is now ready!");
                return Ok(true);
            }
        }
        Err(anyhow::anyhow!("Ollama server started but did not become ready within 60 seconds"))
    } else {
        Err(anyhow::anyhow!("Failed to start Ollama server. Please start it manually with `ollama serve`"))
    }
}

async fn start_ollama_server() -> bool {
    #[cfg(target_os = "windows")]
    {
        start_ollama_windows().await
    }
    #[cfg(target_os = "macos")]
    {
        start_ollama_macos().await
    }
    #[cfg(target_os = "linux")]
    {
        start_ollama_linux().await
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(target_os = "windows")]
async fn start_ollama_windows() -> bool {
    // Try to find ollama.exe
    let ollama_exe = find_ollama_binary("ollama.exe");
    
    if let Some(ollama_path) = ollama_exe {
        // First try to start the tray application (if it exists)
        let tray_path = ollama_path.parent()
            .map(|p| p.join("Ollama.exe"))
            .filter(|p| p.exists());
        
        if let Some(tray) = tray_path {
            let _ = Command::new(&tray)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            // Give it a moment to start
            sleep(Duration::from_secs(2)).await;
            return true;
        }
        
        // Fall back to `ollama serve`
        match Command::new(&ollama_path)
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn() {
            Ok(_) => {
                info!("Started Ollama server on Windows: {:?}", ollama_path);
                true
            }
            Err(e) => {
                warn!("Failed to start ollama serve on Windows: {}", e);
                false
            }
        }
    } else {
        warn!("Ollama binary not found on Windows. Searched PATH and common locations.");
        false
    }
}

#[cfg(target_os = "macos")]
async fn start_ollama_macos() -> bool {
    // Try to find ollama binary
    let ollama_bin = find_ollama_binary("ollama");
    
    if let Some(ollama_path) = ollama_bin {
        // Check if Ollama.app is available and try to launch it
        let app_path = PathBuf::from("/Applications/Ollama.app");
        if app_path.exists() {
            let _ = Command::new("open")
                .arg("-a")
                .arg("Ollama")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            sleep(Duration::from_secs(2)).await;
            return true;
        }
        
        // Fall back to `ollama serve`
        match Command::new(&ollama_path)
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn() {
            Ok(_) => {
                info!("Started Ollama server on macOS: {:?}", ollama_path);
                true
            }
            Err(e) => {
                warn!("Failed to start ollama serve on macOS: {}", e);
                false
            }
        }
    } else {
        warn!("Ollama binary not found on macOS. Searched PATH and common locations.");
        false
    }
}

#[cfg(target_os = "linux")]
async fn start_ollama_linux() -> bool {
    // Try to find ollama binary
    let ollama_bin = find_ollama_binary("ollama");
    
    if let Some(ollama_path) = ollama_bin {
        // Check if systemd service exists and try to start it
        match Command::new("systemctl")
            .args(["--user", "is-active", "ollama"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await {
            Ok(output) if output.status.success() => {
                // Service is already active
                return true;
            }
            _ => {}
        }
        
        // Try to start the user service if available
        match Command::new("systemctl")
            .args(["--user", "start", "ollama"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await {
            Ok(output) if output.status.success() => {
                info!("Started Ollama user service on Linux");
                return true;
            }
            _ => {}
        }
        
        // Try system-wide service
        match Command::new("sudo")
            .args(["systemctl", "start", "ollama"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await {
            Ok(output) if output.status.success() => {
                info!("Started Ollama system service on Linux");
                return true;
            }
            _ => {}
        }
        
        // Fall back to `ollama serve`
        match Command::new(&ollama_path)
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn() {
            Ok(_) => {
                info!("Started Ollama server on Linux: {:?}", ollama_path);
                true
            }
            Err(e) => {
                warn!("Failed to start ollama serve on Linux: {}", e);
                false
            }
        }
    } else {
        warn!("Ollama binary not found on Linux. Searched PATH and common locations.");
        false
    }
}

fn find_ollama_binary(name: &str) -> Option<PathBuf> {
    // Check PATH first
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // Common locations
    let common_paths: Vec<PathBuf> = [
        #[cfg(target_os = "windows")]
        {
            let mut paths = vec![];
            if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                paths.push(PathBuf::from(&local_app_data).join("Programs").join("Ollama").join(name));
            }
            if let Ok(userprofile) = std::env::var("USERPROFILE") {
                paths.push(PathBuf::from(&userprofile).join("AppData").join("Local").join("Programs").join("Ollama").join(name));
            }
            paths.push(PathBuf::from("C:\\Program Files\\Ollama").join(name));
            paths
        },
        #[cfg(target_os = "macos")]
        {
            vec![
                PathBuf::from("/usr/local/bin").join(name),
                PathBuf::from("/opt/homebrew/bin").join(name),
                PathBuf::from("/usr/bin").join(name),
            ]
        },
        #[cfg(target_os = "linux")]
        {
            vec![
                PathBuf::from("/usr/local/bin").join(name),
                PathBuf::from("/usr/bin").join(name),
                PathBuf::from("/bin").join(name),
                PathBuf::from("/snap/bin").join(name),
            ]
        },
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            vec![]
        }
    ].concat();

    for path in common_paths {
        if path.exists() {
            return Some(path);
        }
    }

    None
}

// ============================================================================
// Model Discovery
// ============================================================================

#[derive(Debug, Deserialize, Clone)]
pub struct OllamaModelTag {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub details: Option<OllamaModelDetails>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct OllamaModelDetails {
    #[serde(default)]
    pub parent_model: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub families: Option<Vec<String>>,
    #[serde(default)]
    pub parameter_size: Option<String>,
    #[serde(default)]
    pub quantization_level: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModelTag>,
}

pub async fn discover_ollama_models(base_url: &str) -> Result<Vec<OllamaModelTag>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let url = format!("{}/api/tags", base_url);
    let resp = client.get(&url).send().await
        .context("Failed to fetch Ollama model tags")?;

    if !resp.status().is_success() {
        anyhow::bail!("Ollama /api/tags returned HTTP {}", resp.status());
    }

    let body: OllamaTagsResponse = resp.json().await
        .context("Failed to parse Ollama tags response")?;

    Ok(body.models)
}

// ============================================================================
// Model Metadata / Show
// ============================================================================

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct OllamaModelInfo {
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub modelfile: Option<String>,
    #[serde(default)]
    pub parameters: Option<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub details: Option<OllamaModelDetails>,
    #[serde(default)]
    pub model_info: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Serialize)]
struct OllamaShowRequest<'a> {
    model: &'a str,
}

pub async fn fetch_model_info(base_url: &str, model: &str) -> Result<OllamaModelInfo> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let url = format!("{}/api/show", base_url);
    let resp = client.post(&url)
        .json(&OllamaShowRequest { model })
        .send()
        .await
        .context("Failed to fetch Ollama model info")?;

    if !resp.status().is_success() {
        anyhow::bail!("Ollama /api/show returned HTTP {}", resp.status());
    }

    let info: OllamaModelInfo = resp.json().await
        .context("Failed to parse Ollama show response")?;

    Ok(info)
}

/// Extract context length from model info using Ollama's metadata keys
pub fn extract_context_length(info: &OllamaModelInfo) -> Option<u32> {
    if let Some(ref model_info) = info.model_info {
        // Look for any key ending in ".context_length"
        if let Some((_, val)) = model_info.iter()
            .find(|(k, _)| k.ends_with(".context_length")) {
            return val.as_u64().map(|v| v as u32);
        }
    }
    None
}

/// Extract architecture from model info
pub fn extract_architecture(info: &OllamaModelInfo) -> Option<String> {
    if let Some(ref model_info) = info.model_info {
        if let Some(val) = model_info.get("general.architecture") {
            return val.as_str().map(|s| s.to_string());
        }
    }
    info.details.as_ref().and_then(|d| d.family.clone())
}

/// Extract parameter count from model info
pub fn extract_parameter_count(info: &OllamaModelInfo) -> Option<String> {
    if let Some(ref model_info) = info.model_info {
        if let Some(val) = model_info.get("general.parameter_count") {
            return val.as_u64().map(|n| format_parameter_size(n));
        }
    }
    info.details.as_ref().and_then(|d| d.parameter_size.clone())
}

pub fn format_parameter_size(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

// ============================================================================
// Modelfile Parsing
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct ParsedModelfile {
    pub from: Option<String>,
    pub system: Option<String>,
    pub parameters: HashMap<String, String>,
    pub template: Option<String>,
    pub stop_sequences: Vec<String>,
    pub messages: Vec<(String, String)>, // role, content
}

pub fn parse_modelfile(content: &str) -> ParsedModelfile {
    let mut parsed = ParsedModelfile::default();
    let mut current_block = String::new();
    let mut in_multiline = false;
    let mut multiline_keyword = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Handle multiline blocks (""" delimited)
        if in_multiline {
            if trimmed.ends_with("\"\"\"") {
                let end = trimmed[..trimmed.len()-3].trim();
                current_block.push_str(end);
                match multiline_keyword.as_str() {
                    "SYSTEM" => parsed.system = Some(current_block.trim().to_string()),
                    "TEMPLATE" => parsed.template = Some(current_block.trim().to_string()),
                    _ => {}
                }
                in_multiline = false;
                current_block.clear();
            } else {
                current_block.push_str(line);
                current_block.push('\n');
            }
            continue;
        }

        // Check for multiline start
        if let Some(pos) = trimmed.find('"') {
            let keyword = trimmed[..pos].trim().to_uppercase();
            let rest = trimmed[pos..].trim();
            if rest.starts_with("\"\"\"") {
                in_multiline = true;
                multiline_keyword = keyword.clone();
                let start = rest[3..].trim();
                if start.ends_with("\"\"\"") {
                    // Single-line triple-quote
                    let content = start[..start.len()-3].trim();
                    match keyword.as_str() {
                        "SYSTEM" => parsed.system = Some(content.to_string()),
                        "TEMPLATE" => parsed.template = Some(content.to_string()),
                        _ => {}
                    }
                    in_multiline = false;
                } else {
                    current_block.push_str(start);
                    current_block.push('\n');
                }
                continue;
            }
        }

        // Single-line instructions
        let upper = trimmed.to_uppercase();
        if upper.starts_with("FROM ") {
            parsed.from = Some(trimmed[5..].trim().to_string());
        } else if upper.starts_with("SYSTEM ") {
            parsed.system = Some(trimmed[7..].trim().to_string());
        } else if upper.starts_with("TEMPLATE ") {
            parsed.template = Some(trimmed[9..].trim().to_string());
        } else if upper.starts_with("PARAMETER ") {
            let rest = trimmed[10..].trim();
            let mut parts = rest.splitn(2, char::is_whitespace);
            if let (Some(key), Some(val)) = (parts.next(), parts.next()) {
                parsed.parameters.insert(key.to_string(), val.to_string());
            }
        } else if upper.starts_with("STOP ") {
            parsed.stop_sequences.push(trimmed[5..].trim().to_string());
        } else if upper.starts_with("MESSAGE ") {
            let rest = trimmed[8..].trim();
            let mut parts = rest.splitn(2, char::is_whitespace);
            if let (Some(role), Some(content)) = (parts.next(), parts.next()) {
                parsed.messages.push((role.to_string(), content.to_string()));
            }
        }
    }

    parsed
}

// ============================================================================
// Model Info Cache
// ============================================================================

#[derive(Debug, Clone)]
pub struct CachedModelInfo {
    pub info: OllamaModelInfo,
    pub discovered_at: std::time::Instant,
    pub context_length: u32,
    pub supports_thinking: bool,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

pub struct ModelInfoCache {
    cache: RwLock<HashMap<String, CachedModelInfo>>,
    ttl: Duration,
}

impl ModelInfoCache {
    pub fn new(ttl_sec: u64) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_sec),
        }
    }

    pub fn get(&self, model: &str) -> Option<CachedModelInfo> {
        let guard = self.cache.read().ok()?;
        let entry = guard.get(model)?;
        if entry.discovered_at.elapsed() < self.ttl {
            Some(entry.clone())
        } else {
            None
        }
    }

    pub fn insert(&self, model: &str, info: CachedModelInfo) {
        if let Ok(mut guard) = self.cache.write() {
            guard.insert(model.to_string(), info);
        }
    }

    pub fn invalidate(&self, model: &str) {
        if let Ok(mut guard) = self.cache.write() {
            guard.remove(model);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.cache.write() {
            guard.clear();
        }
    }

    pub fn all_models(&self) -> Vec<String> {
        let guard = self.cache.read().ok();
        match guard {
            Some(g) => g.keys().cloned().collect(),
            None => vec![],
        }
    }
}

// ============================================================================
// Thinking Support
// ============================================================================

/// Returns true if the model name suggests native thinking capability.
pub fn model_supports_thinking(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("deepseek-r1")
        || m.contains("deepseek-v3.1")
        || m.contains("deepseek-v3")
        || m.contains("qwq")
        || m.contains("qwen3")
        || m.contains("marco-o1")
        || m.contains("cogito")
        || m.contains("exaone-deep")
        || m.contains("gpt-oss")
        || m.contains("thinking")
}

/// Resolve the thinking parameter based on model and config.
/// Returns None if thinking should not be sent.
/// Returns Some(Value) for the think parameter.
pub fn resolve_thinking_param(model: &str, mode: ThinkingMode) -> Option<serde_json::Value> {
    match mode {
        ThinkingMode::Disabled => None,
        ThinkingMode::Auto => {
            if model_supports_thinking(model) {
                Some(serde_json::Value::Bool(true))
            } else {
                None
            }
        }
        ThinkingMode::Enabled => {
            Some(serde_json::Value::Bool(true))
        }
        ThinkingMode::Low => Some(serde_json::Value::String("low".to_string())),
        ThinkingMode::Medium => Some(serde_json::Value::String("medium".to_string())),
        ThinkingMode::High => Some(serde_json::Value::String("high".to_string())),
        ThinkingMode::Max => Some(serde_json::Value::String("max".to_string())),
    }
}

// ============================================================================
// Structured Output
// ============================================================================

/// Generate Ollama format parameter for structured JSON output.
/// For simple JSON mode, returns "json".
/// For schema-constrained, returns the schema object.
pub fn build_structured_format(schema: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    match schema {
        Some(schema) => Some(schema.clone()),
        None => Some(serde_json::Value::String("json".to_string())),
    }
}

// ============================================================================
// Tool Calling
// ============================================================================

/// Convert a Hydragent tool schema to Ollama's native tool format.
/// Ollama uses OpenAI-compatible function calling format.
pub fn build_ollama_tool(name: &str, description: &str, params_schema: &str) -> Option<serde_json::Value> {
    let schema: serde_json::Value = serde_json::from_str(params_schema).ok()?;
    Some(serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": schema
        }
    }))
}

/// Parse tool calls from Ollama's streaming response chunks.
#[derive(Debug, Deserialize, Clone)]
pub struct OllamaToolCall {
    #[serde(default)]
    pub function: OllamaToolFunction,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct OllamaToolFunction {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

// ============================================================================
// Streaming Enhancements
// ============================================================================

/// Buffered token sender that batches tokens for smoother UI experience.
pub struct TokenBuffer {
    buffer: String,
    last_send: std::time::Instant,
    buffer_ms: u64,
}

impl TokenBuffer {
    pub fn new(buffer_ms: u64) -> Self {
        Self {
            buffer: String::new(),
            last_send: std::time::Instant::now(),
            buffer_ms,
        }
    }

    pub async fn send(&mut self, token: &str, tx: &mpsc::Sender<String>) -> Result<()> {
        self.buffer.push_str(token);
        let elapsed = self.last_send.elapsed().as_millis() as u64;
        if elapsed >= self.buffer_ms || self.buffer.len() >= 64 || token.contains('\n') {
            let to_send = std::mem::take(&mut self.buffer);
            tx.send(to_send).await
                .map_err(|_| anyhow::anyhow!("Token channel closed"))?;
            self.last_send = std::time::Instant::now();
        }
        Ok(())
    }

    pub async fn flush(&mut self, tx: &mpsc::Sender<String>) -> Result<()> {
        if !self.buffer.is_empty() {
            let to_send = std::mem::take(&mut self.buffer);
            tx.send(to_send).await
                .map_err(|_| anyhow::anyhow!("Token channel closed"))?;
        }
        Ok(())
    }
}

// ============================================================================
// Model Definition Builder for YAML Registry
// ============================================================================

/// Convert an Ollama discovered model tag into a ModelDefinition-compatible entry.
/// This is used to auto-populate the model_providers.yaml with discovered models.
pub fn build_model_definition_from_tag(tag: &OllamaModelTag) -> serde_json::Value {
    let name = tag.name.clone();
    let id = name.replace(":", "-").replace(".", "-");
    let api_id = name.clone();
    
    let _family = tag.details.as_ref()
        .and_then(|d| d.family.clone())
        .unwrap_or_else(|| "unknown".to_string());
    
    let param_size = tag.details.as_ref()
        .and_then(|d| d.parameter_size.clone())
        .unwrap_or_default();
    
    let display_name = if param_size.is_empty() {
        name.clone()
    } else {
        format!("{} ({})", name, param_size)
    };

    // Infer capabilities based on model name patterns
    let tool_calling = model_supports_tools(&name);
    let vision = model_supports_vision(&name);
    let reasoning = model_supports_thinking(&name);
    
    // Estimate context window based on parameter size
    let context_window = estimate_context_window(&param_size, &name);

    serde_json::json!({
        "id": id,
        "name": display_name,
        "aliases": [name.clone()],
        "api_model_id": api_id,
        "tool_calling": tool_calling,
        "vision": vision,
        "reasoning": reasoning,
        "streaming": true,
        "max_input_tokens": context_window,
        "max_output_tokens": context_window / 2,
        "cost_per_1k": 0.0,
        "cost_tier": "free"
    })
}

pub fn model_supports_tools(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("qwen") || n.contains("llama3") || n.contains("llama-3") ||
    n.contains("mistral") || n.contains("mixtral") || n.contains("gemma4") ||
    n.contains("command") || n.contains("phi4") || n.contains("deepseek")
}

pub fn model_supports_vision(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("vision") || n.contains("llava") || n.contains("gemma4") ||
    n.contains("moondream") || n.contains("bakllava") || n.contains("multimodal")
}

pub fn estimate_context_window(param_size: &str, name: &str) -> u32 {
    // Use parameter size hints
    if param_size.contains("405B") || param_size.contains("400B") {
        return 128_000;
    }
    if param_size.contains("70B") {
        return 128_000;
    }
    if param_size.contains("32B") || param_size.contains("34B") {
        return 128_000;
    }
    if param_size.contains("8B") || param_size.contains("7B") {
        return 128_000;
    }
    if param_size.contains("3B") || param_size.contains("4B") {
        return 128_000;
    }
    
    // Fallback based on model name patterns
    let n = name.to_lowercase();
    if n.contains("qwen2.5") || n.contains("qwen3") {
        return 128_000;
    }
    if n.contains("llama3") || n.contains("llama-3") {
        return 128_000;
    }
    if n.contains("mistral") || n.contains("mixtral") {
        return 32_000;
    }
    if n.contains("gemma4") {
        return 128_000;
    }
    if n.contains("phi4") || n.contains("phi-4") {
        return 128_000;
    }
    
    8192
}

// ============================================================================
// YAML Registry Sync
// ============================================================================

/// Update model_providers.yaml with discovered Ollama models.
/// Returns true if the file was modified.
pub fn sync_discovered_models_to_yaml(
    yaml_path: &std::path::Path,
    discovered: &[OllamaModelTag],
) -> Result<bool> {
    if !yaml_path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(yaml_path)?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&content)?;

    let mut modified = false;

    if let Some(providers) = doc.get_mut("providers").and_then(|p| p.as_sequence_mut()) {
        for provider in providers.iter_mut() {
            let kind = provider.get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or("");
            let id = provider.get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("");
            
            if kind == "ollama" || id == "ollama" {
                // Ensure models list exists before borrowing
                if provider.get("models").is_none() {
                    if let serde_yaml::Value::Mapping(ref mut map) = provider {
                        map.insert("models".into(), serde_yaml::Value::Sequence(vec![]));
                    }
                }

                let existing_models = provider.get_mut("models")
                    .and_then(|m| m.as_sequence_mut());
                
                let existing_models = match existing_models {
                    Some(m) => m,
                    None => break,
                };
                
                let existing_ids: std::collections::HashSet<String> = existing_models
                    .iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                    .collect();
                
                for tag in discovered {
                    let model_def = build_model_definition_from_tag(tag);
                    let new_id = model_def.get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("");
                    
                    if !existing_ids.contains(new_id) {
                        if let Ok(yaml_val) = serde_yaml::to_value(&model_def) {
                            existing_models.push(yaml_val);
                            modified = true;
                            info!("Added discovered Ollama model '{}' to registry", tag.name);
                        }
                    }
                }
                
                break;
            }
        }
    }

    if modified {
        let new_content = serde_yaml::to_string(&doc)?;
        std::fs::write(yaml_path, new_content)?;
    }

    Ok(modified)
}

// ============================================================================
// Native Ollama Client
// ============================================================================

pub struct OllamaNativeClient {
    config: OllamaNativeConfig,
    client: Client,
    info_cache: Arc<ModelInfoCache>,
}

impl OllamaNativeClient {
    pub fn new(config: OllamaNativeConfig) -> Self {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_else(|_| Client::new());
        
        let info_cache = Arc::new(ModelInfoCache::new(config.model_metadata_ttl_sec));
        
        Self {
            config,
            client,
            info_cache,
        }
    }

    pub fn from_env() -> Self {
        Self::new(OllamaNativeConfig::from_env())
    }

    pub fn config(&self) -> &OllamaNativeConfig {
        &self.config
    }

    pub fn info_cache(&self) -> &Arc<ModelInfoCache> {
        &self.info_cache
    }

    pub async fn ensure_server(&self) -> Result<bool> {
        if self.config.auto_serve {
            ensure_ollama_server(&self.config.base_url).await
        } else {
            Ok(matches!(
                check_ollama_server(&self.config.base_url).await,
                OllamaServerStatus::Running
            ))
        }
    }

    pub async fn discover_models(&self) -> Result<Vec<OllamaModelTag>> {
        discover_ollama_models(&self.config.base_url).await
    }

    pub async fn get_model_info(&self, model: &str) -> Result<CachedModelInfo> {
        // Check cache first
        if let Some(cached) = self.info_cache.get(model) {
            return Ok(cached);
        }

        let info = fetch_model_info(&self.config.base_url, model).await?;
        let context_length = self.config.context_length_override
            .or_else(|| extract_context_length(&info))
            .unwrap_or(self.config.default_num_ctx);
        
        let supports_thinking = model_supports_thinking(model);
        let supports_tools = model_supports_tools(model);
        let supports_vision = model_supports_vision(model);

        let cached = CachedModelInfo {
            info: info.clone(),
            discovered_at: std::time::Instant::now(),
            context_length,
            supports_thinking,
            supports_tools,
            supports_vision,
        };

        self.info_cache.insert(model, cached.clone());
        Ok(cached)
    }

    pub async fn warmup_model(&self, model: &str) -> Result<bool> {
        let url = format!("{}/api/generate", self.config.base_url);
        
        let body = serde_json::json!({
            "model": model,
            "keep_alive": -1
        });

        let resp = self.client.post(&url)
            .json(&body)
            .timeout(Duration::from_secs(60))
            .send()
            .await?;

        if resp.status().is_success() {
            info!("Ollama model '{}' loaded into VRAM", model);
            Ok(true)
        } else {
            warn!("Ollama model '{}' load failed: HTTP {}", model, resp.status());
            Ok(false)
        }
    }

    pub async fn stream_completion(
        &self,
        request: &LLMRequest,
        tx: mpsc::Sender<String>,
    ) -> Result<String> {
        let model = if request.model.is_empty() {
            &self.config.default_model
        } else {
            &request.model
        };

        // Ensure server is running
        if self.config.auto_serve {
            let _ = self.ensure_server().await;
        }

        // Fast intercept for background warmup requests
        let is_warmup = request.messages.last().map(|m| m.content.as_str()) == Some("warmup") && request.max_tokens == Some(1);
        if is_warmup {
            let start = std::time::Instant::now();
            let success = self.warmup_model(model).await.unwrap_or(false);
            if success {
                let load_dur = start.elapsed().as_millis();
                println!(
                    "\n  ✓ Brain cache warm [Model loaded in VRAM] · load: {}ms",
                    load_dur
                );
                let _ = tx.send("".to_string()).await;
                return Ok("".to_string());
            }
        }

        // Get model info for context window and capabilities
        let model_info = self.get_model_info(model).await.ok();
        let num_ctx = model_info.as_ref()
            .map(|i| i.context_length)
            .unwrap_or(self.config.default_num_ctx);

        // Build options
        let mut options = serde_json::Map::new();
        options.insert("num_ctx".to_string(), serde_json::Value::Number(num_ctx.into()));
        
        if let Some(t) = self.config.num_thread {
            options.insert("num_thread".to_string(), serde_json::Value::Number(t.into()));
        }
        if let Some(t) = request.max_tokens {
            options.insert("num_predict".to_string(), serde_json::Value::Number((t as i64).into()));
        }
        if let Some(t) = self.config.temperature {
            options.insert("temperature".to_string(), serde_json::Value::Number(serde_json::Number::from_f64(t as f64).unwrap_or(0.into())));
        }
        if let Some(r) = self.config.repeat_penalty {
            options.insert("repeat_penalty".to_string(), serde_json::Value::Number(serde_json::Number::from_f64(r as f64).unwrap_or(0.into())));
        }
        if let Some(p) = self.config.top_p {
            options.insert("top_p".to_string(), serde_json::Value::Number(serde_json::Number::from_f64(p as f64).unwrap_or(0.into())));
        }
        if let Some(k) = self.config.top_k {
            options.insert("top_k".to_string(), serde_json::Value::Number(k.into()));
        }

        // Build request body
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), serde_json::Value::String(model.to_string()));
        body.insert("messages".to_string(), serde_json::to_value(&request.messages)?);
        body.insert("stream".to_string(), serde_json::Value::Bool(true));
        body.insert("options".to_string(), serde_json::Value::Object(options));

        // Keep alive
        if let Some(ref ka) = self.config.keep_alive {
            body.insert("keep_alive".to_string(), serde_json::Value::String(ka.clone()));
        }

        // Thinking
        if let Some(think) = resolve_thinking_param(model, self.config.thinking_mode) {
            body.insert("think".to_string(), think);
        }

        // Structured output (for JSON tool responses)
        if self.config.structured_output && request.messages.iter().any(|m| {
            m.content.contains("JSON") || m.content.contains("json")
        }) {
            // Only set format if the model is likely to support it
            if model_supports_tools(model) || model_supports_thinking(model) {
                body.insert("format".to_string(), serde_json::Value::String("json".to_string()));
            }
        }

        // Native tool calling - if enabled and available
        if self.config.native_tool_calling {
            // This would be populated from the tool registry in practice
            // For now, we leave it empty unless a specific tool schema is provided
        }

        let url = format!("{}/api/chat", self.config.base_url);
        let resp = self.client
            .post(&url)
            .json(&serde_json::Value::Object(body))
            .send()
            .await
            .context("Ollama native provider request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Ollama native provider error response ({}): {}",
                status,
                error_text
            );
        }

        let mut full_content = String::new();
        let mut full_thinking = String::new();
        let mut stream = resp.bytes_stream();
        let mut byte_buffer: Vec<u8> = Vec::new();
        let mut in_thinking = false;
        let mut token_buffer = TokenBuffer::new(self.config.stream_buffer_ms);

        info!("Ollama native streaming started for model: {}", model);

        use tokio_stream::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("Ollama chunk error")?;
            byte_buffer.extend_from_slice(&bytes);

            while let Some(newline_idx) = byte_buffer.iter().position(|&b| b == b'\n') {
                let line_bytes = byte_buffer[..newline_idx].to_vec();
                byte_buffer.drain(..=newline_idx);

                let line = String::from_utf8(line_bytes)?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                match serde_json::from_str::<OllamaNativeChatResponseChunk>(&trimmed) {
                    Ok(chunk_data) => {
                        // Handle thinking
                        if let Some(ref msg) = chunk_data.message {
                            if let Some(ref thinking) = msg.thinking {
                                if !thinking.is_empty() {
                                    if !in_thinking {
                                        in_thinking = true;
                                        let _ = token_buffer.send("<think>", &tx).await;
                                    }
                                    full_thinking.push_str(thinking);
                                    let _ = token_buffer.send(thinking, &tx).await;
                                }
                            }
                            if let Some(ref content) = msg.content {
                                if !content.is_empty() {
                                    if in_thinking {
                                        in_thinking = false;
                                        let _ = token_buffer.send("</think>", &tx).await;
                                    }
                                    full_content.push_str(content);
                                    let _ = token_buffer.send(content, &tx).await;
                                }
                            }
                            // Handle native tool calls
                            if let Some(ref tool_calls) = msg.tool_calls {
                                for tc in tool_calls {
                                    let tool_json = serde_json::to_string(tc).unwrap_or_default();
                                    full_content.push_str(&format!("\n[tool_call] {}\n", tool_json));
                                }
                            }
                        }

                        if chunk_data.done {
                            if in_thinking {
                                let _ = token_buffer.send("</think>", &tx).await;
                            }
                            token_buffer.flush(&tx).await.ok();
                            
                            let prompt_tokens = chunk_data.prompt_eval_count.unwrap_or(0);
                            let completion_tokens = chunk_data.eval_count.unwrap_or(0);
                            let duration_ms = chunk_data.total_duration.map(|ns| ns / 1_000_000).unwrap_or(0);
                            info!(
                                model = %model,
                                prompt_tokens = %prompt_tokens,
                                completion_tokens = %completion_tokens,
                                duration_ms = %duration_ms,
                                "Ollama native generation completed"
                            );

                            let is_warmup = request.messages.last().map(|m| m.content.as_str()) == Some("warmup") && request.max_tokens == Some(1);
                            if is_warmup {
                                let load_dur = chunk_data.load_duration.map(|ns| ns / 1_000_000).unwrap_or(0);
                                let eval_dur = chunk_data.prompt_eval_duration.map(|ns| ns / 1_000_000).unwrap_or(0);
                                let is_cached = eval_dur < 100;
                                println!(
                                    "\n  ✓ Brain cache warm · prompt: {} tokens · load: {}ms · eval: {}ms {}",
                                    prompt_tokens,
                                    load_dur,
                                    eval_dur,
                                    if is_cached { "[KV cache HIT]" } else { "[KV cache MISS]" }
                                );
                            }

                            break;
                        }
                    }
                    Err(e) => {
                        info!("Ollama native parse skip: {} | {}", e, &trimmed[..trimmed.len().min(120)]);
                    }
                }
            }
        }

        // Process trailing bytes
        if !byte_buffer.is_empty() {
            if let Ok(line) = String::from_utf8(byte_buffer) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    if let Ok(chunk_data) = serde_json::from_str::<OllamaNativeChatResponseChunk>(&trimmed) {
                        if let Some(ref msg) = chunk_data.message {
                            if let Some(ref content) = msg.content {
                                if !content.is_empty() {
                                    full_content.push_str(content);
                                    let _ = tx.send(content.clone()).await;
                                }
                            }
                        }
                    }
                }
            }
        }

        token_buffer.flush(&tx).await.ok();

        // Return combined content (thinking + content for backward compat)
        if full_thinking.is_empty() {
            Ok(full_content)
        } else {
            Ok(format!("<think>\n{}\n</think>\n{}", full_thinking, full_content))
        }
    }
}

#[async_trait::async_trait]
impl ModelProvider for OllamaNativeClient {
    fn provider_name(&self) -> &str {
        "ollama"
    }

    fn is_available(&self) -> bool {
        !self.config.base_url.is_empty()
    }

    async fn chat_stream(
        &self,
        request: &LLMRequest,
        token_tx: mpsc::Sender<String>,
    ) -> Result<String> {
        self.stream_completion(request, token_tx).await
    }
}

// ============================================================================
// Request/Response Types
// ============================================================================

#[derive(Debug, Deserialize)]
struct OllamaNativeChatResponseChunk {
    message: Option<OllamaNativeMessageChunk>,
    #[serde(default)]
    done: bool,
    prompt_eval_count: Option<u32>,
    eval_count: Option<u32>,
    total_duration: Option<u64>,
    load_duration: Option<u64>,
    prompt_eval_duration: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
struct OllamaNativeMessageChunk {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thinking_mode_parsing() {
        assert!(matches!(resolve_thinking_param("deepseek-r1", ThinkingMode::Auto), Some(_)));
        assert!(resolve_thinking_param("llama3.1", ThinkingMode::Auto).is_none());
        assert!(resolve_thinking_param("llama3.1", ThinkingMode::Disabled).is_none());
        assert!(resolve_thinking_param("llama3.1", ThinkingMode::Enabled).is_some());
        
        let low = resolve_thinking_param("gpt-oss", ThinkingMode::Low).unwrap();
        assert_eq!(low, serde_json::Value::String("low".to_string()));
    }

    #[test]
    fn test_model_supports_thinking() {
        assert!(model_supports_thinking("deepseek-r1:8b"));
        assert!(model_supports_thinking("qwen3:latest"));
        assert!(model_supports_thinking("gpt-oss:120b"));
        assert!(!model_supports_thinking("llama3.1:8b"));
        assert!(!model_supports_thinking("mistral:latest"));
    }

    #[test]
    fn test_model_supports_tools() {
        assert!(model_supports_tools("qwen2.5-coder:32b"));
        assert!(model_supports_tools("llama3.1:8b"));
        assert!(model_supports_tools("gemma4:latest"));
    }

    #[test]
    fn test_model_supports_vision() {
        assert!(model_supports_vision("llava:13b"));
        assert!(model_supports_vision("gemma4:latest"));
        assert!(!model_supports_vision("llama3.1:8b"));
    }

    #[test]
    fn test_estimate_context_window() {
        assert_eq!(estimate_context_window("8.0B", "llama3.1"), 128_000);
        assert_eq!(estimate_context_window("", "mistral:latest"), 32_000);
        assert_eq!(estimate_context_window("", "unknown"), 8192);
    }

    #[test]
    fn test_format_parameter_size() {
        assert_eq!(format_parameter_size(8_000_000_000), "8.0B");
        assert_eq!(format_parameter_size(7_000_000), "7.0M");
        assert_eq!(format_parameter_size(500), "500");
    }

    #[test]
    fn test_parse_modelfile() {
        let content = r#"
FROM llama3.2
PARAMETER temperature 0.7
PARAMETER num_ctx 4096
SYSTEM You are a helpful assistant.
STOP "<|end|>"
TEMPLATE """{{ .System }}
{{ .Prompt }}"""
"#;
        let parsed = parse_modelfile(content);
        assert_eq!(parsed.from, Some("llama3.2".to_string()));
        assert_eq!(parsed.system, Some("You are a helpful assistant.".to_string()));
        assert_eq!(parsed.parameters.get("temperature"), Some(&"0.7".to_string()));
        assert_eq!(parsed.parameters.get("num_ctx"), Some(&"4096".to_string()));
        assert_eq!(parsed.stop_sequences, vec!["\"<|end|>\""]);
        assert!(parsed.template.is_some());
    }

    #[test]
    fn test_parse_modelfile_multiline() {
        let content = r#"SYSTEM """You are a
helpful assistant.
""""#;
        let parsed = parse_modelfile(content);
        assert_eq!(parsed.system, Some("You are a\nhelpful assistant.".to_string()));
    }

    #[test]
    fn test_extract_context_length() {
        let mut info = OllamaModelInfo::default();
        assert!(extract_context_length(&info).is_none());
        
        let mut map = HashMap::new();
        map.insert("llama.context_length".to_string(), serde_json::Value::Number(128000.into()));
        info.model_info = Some(map);
        assert_eq!(extract_context_length(&info), Some(128000));
    }

    #[test]
    fn test_build_ollama_tool() {
        let tool = build_ollama_tool("get_weather", "Get weather", r#"{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}"#);
        assert!(tool.is_some());
        let t = tool.unwrap();
        assert_eq!(t["function"]["name"], "get_weather");
    }

    #[test]
    fn test_build_model_definition_from_tag() {
        let tag = OllamaModelTag {
            name: "llama3.1:8b".to_string(),
            model: "llama3.1:8b".to_string(),
            modified_at: None,
            size: Some(3825819519),
            digest: None,
            details: Some(OllamaModelDetails {
                parent_model: None,
                format: Some("gguf".to_string()),
                family: Some("llama".to_string()),
                families: None,
                parameter_size: Some("8.0B".to_string()),
                quantization_level: Some("Q4_0".to_string()),
            }),
        };
        let def = build_model_definition_from_tag(&tag);
        assert_eq!(def["id"], "llama3-1-8b");
        assert_eq!(def["api_model_id"], "llama3.1:8b");
        assert_eq!(def["cost_tier"], "free");
        assert_eq!(def["max_input_tokens"], 128000);
    }

    #[test]
    fn test_token_buffer() {
        // TokenBuffer requires async, so we just test the struct creation
        let buf = TokenBuffer::new(10);
        assert_eq!(buf.buffer_ms, 10);
    }
}
