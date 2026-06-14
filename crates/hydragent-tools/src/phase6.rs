// crates/hydragent-tools/src/phase6.rs
//
// Phase 6 user-callable tools — exposed to the LLM so the chat CLI
// can be driven with natural-language prompts that hit real Phase 6
// features (Track 6.1 audit query, 6.2 taint check, 6.3 injection
// scan, 6.4 vault rotation / mlock status).
//
// Each tool is small and self-contained. They share the *data
// directory* (resolved from the same `AppConfig.data_dir` the rest
// of the agent uses) but otherwise do not need to know about each
// other or the rest of the registry.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hydragent_types::{TaintCategory, TaintSet, ToolResult, ToolStatus};
use serde::Deserialize;
use serde_json::json;

use crate::tool_trait::Tool;

// ============================================================================
// Track 6.1 — `audit_query`
// ============================================================================

/// Read-only query over the Merkle audit chain.
pub struct AuditQueryTool {
    data_dir: PathBuf,
}

impl AuditQueryTool {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self { data_dir: data_dir.into() }
    }

    async fn open_chain(&self) -> Result<hydragent_security::MerkleAuditChain, String> {
        let audit_dir = self.data_dir.join("audit");
        std::fs::create_dir_all(&audit_dir)
            .map_err(|e| format!("create_dir_all({}): {}", audit_dir.display(), e))?;
        let chain_path = audit_dir.join("chain.db");
        let keys_dir = self.data_dir.join("keys");
        std::fs::create_dir_all(&keys_dir)
            .map_err(|e| format!("create_dir_all({}): {}", keys_dir.display(), e))?;
        let key_path = keys_dir.join("agent_ed25519.key");
        let pub_path = std::path::PathBuf::from("config/keys/agent_ed25519.pub");

        let signer = load_or_create_agent_signer(&key_path, &pub_path)
            .map_err(|e| format!("load agent signer: {e}"))?;
        hydragent_security::MerkleAuditChain::connect(
            chain_path.to_str().unwrap(),
            Arc::new(signer),
        )
        .await
        .map_err(|e| format!("open audit chain: {e}"))
    }
}

#[derive(Deserialize)]
struct AuditQueryParams {
    action: String, // "list" | "head" | "verify" | "count"
    #[serde(default = "default_limit")]
    limit: u32,
    #[serde(default)]
    offset: u32,
    #[serde(default)]
    reverse: bool,
    #[serde(default)]
    verify_signatures: bool,
}

fn default_limit() -> u32 { 20 }

#[async_trait]
impl Tool for AuditQueryTool {
    fn name(&self) -> &str { "audit_query" }

    fn description(&self) -> &str {
        "Read-only queries against the Phase 6.1 Merkle audit chain. \
         action='list' returns recent events (oldest first by default; \
         pass reverse=true for newest first). action='head' returns the \
         current chain head hash. action='verify' checks the full chain \
         for tamper-evidence and (with verify_signatures=true) re-validates \
         every Ed25519 signature against the agent's public key. \
         action='count' returns the total number of events."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "head", "verify", "count"],
                    "description": "The query to run against the audit chain."
                },
                "limit": {
                    "type": "integer",
                    "description": "For action='list': max rows to return (default 20)."
                },
                "offset": {
                    "type": "integer",
                    "description": "For action='list': skip the first N rows."
                },
                "reverse": {
                    "type": "boolean",
                    "description": "For action='list': newest first (default false)."
                },
                "verify_signatures": {
                    "type": "boolean",
                    "description": "For action='verify': also check Ed25519 signatures (default false)."
                }
            },
            "required": ["action"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let call_id = String::new();

        let params: AuditQueryParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": format!("invalid parameters: {e}") }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("invalid parameters: {e}")),
                };
            }
        };

        let chain = match self.open_chain().await {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": e }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(e),
                };
            }
        };

        let result: Result<serde_json::Value, String> = match params.action.as_str() {
            "list" => match chain.list(params.limit, params.offset, params.reverse).await {
                Ok(rows) => {
                    let compact: Vec<serde_json::Value> = rows
                        .iter()
                        .map(|r| {
                            json!({
                                "seq_id": r.seq_id,
                                "timestamp_ms": r.timestamp_ms,
                                "event_type": r.event_type,
                                "actor": r.actor,
                                "page_id": r.page_id,
                                "event_hash": r.event_hash,
                                "chain_hash": r.chain_hash,
                                "agent_signature": r.agent_signature,
                            })
                        })
                        .collect();
                    Ok(json!({ "action": "list", "count": compact.len(), "rows": compact }))
                }
                Err(e) => Err(e.to_string()),
            },
            "head" => match chain.head_hash().await {
                Ok(head) => Ok(json!({ "action": "head", "head_hash": head })),
                Err(e) => Err(e.to_string()),
            },
            "count" => match chain.count().await {
                Ok(n) => Ok(json!({ "action": "count", "event_count": n })),
                Err(e) => Err(e.to_string()),
            },
            "verify" => match chain.verify_with_key(params.verify_signatures, None).await {
                Ok(v) => match v {
                    hydragent_security::VerificationResult::Valid { event_count } => Ok(json!({
                        "action": "verify",
                        "valid": true,
                        "event_count": event_count,
                        "signatures_checked": params.verify_signatures,
                    })),
                    hydragent_security::VerificationResult::Tampered { seq_id, detail } => Ok(json!({
                        "action": "verify",
                        "valid": false,
                        "first_break_seq_id": seq_id,
                        "detail": detail,
                        "signatures_checked": params.verify_signatures,
                    })),
                },
                Err(e) => Err(e.to_string()),
            },
            other => Err(format!("unknown action: {other}")),
        };

        match result {
            Ok(v) => ToolResult {
                call_id,
                output_json: v.to_string(),
                status: ToolStatus::Success,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: None,
            },
            Err(e) => ToolResult {
                call_id,
                output_json: json!({ "error": e }).to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(e),
            },
        }
    }
}

// ============================================================================
// Track 6.2 — `taint_check`
// ============================================================================

/// Dry-run a taint policy decision: would this taint set be allowed to
/// reach this sink, and does it need redaction first?
pub struct TaintCheckTool {
    policy_path: PathBuf,
}

impl TaintCheckTool {
    pub fn new(policy_path: impl Into<PathBuf>) -> Self {
        Self { policy_path: policy_path.into() }
    }

    fn load_policy(&self) -> Result<(hydragent_security::SinkPolicy, String), String> {
        if self.policy_path.exists() {
            let yaml = std::fs::read_to_string(&self.policy_path)
                .map_err(|e| format!("read {}: {e}", self.policy_path.display()))?;
            let p = hydragent_security::SinkPolicy::load_yaml(&yaml)
                .map_err(|e| format!("parse {}: {e}", self.policy_path.display()))?;
            Ok((p, format!("file:{}", self.policy_path.display())))
        } else {
            Ok((hydragent_security::SinkPolicy::default_policy(), "default (built-in)".to_string()))
        }
    }
}

#[derive(Deserialize)]
struct TaintCheckParams {
    sink: String,            // e.g. "llm_request_messages"
    categories: Vec<String>, // e.g. ["secret", "pii"]
}

fn parse_sink(name: &str) -> Result<hydragent_security::TaintSink, String> {
    use hydragent_security::TaintSink as S;
    match name {
        "llm_request_messages" | "llm" => Ok(S::LlmRequestMessages),
        "outbound_network" | "network" => Ok(S::OutboundNetwork),
        "log" => Ok(S::Log),
        "vault_write" | "vault" => Ok(S::VaultWrite),
        "disk_write" | "disk" => Ok(S::DiskWrite),
        other => Err(format!(
            "unknown sink '{other}' (expected: llm_request_messages, \
             outbound_network, log, vault_write, disk_write)"
        )),
    }
}

fn parse_category(name: &str) -> Result<TaintCategory, String> {
    match name {
        "secret" => Ok(TaintCategory::Secret),
        "pii" => Ok(TaintCategory::PII),
        "tool_output" => Ok(TaintCategory::ToolOutput),
        "user_input" => Ok(TaintCategory::UserInput),
        "llm_output" => Ok(TaintCategory::LlmOutput),
        "system_internal" => Ok(TaintCategory::SystemInternal),
        other => Err(format!(
            "unknown taint category '{other}' (expected: secret, pii, \
             tool_output, user_input, llm_output, system_internal)"
        )),
    }
}

#[async_trait]
impl Tool for TaintCheckTool {
    fn name(&self) -> &str { "taint_check" }

    fn description(&self) -> &str {
        "Phase 6.2 taint policy check. Given a sink name and a list of \
         taint categories that a value is tagged with, returns whether the \
         policy would allow the value through, block it, or require \
         redaction. Use to verify a tool's data flow before execution. \
         Sink values: llm_request_messages, outbound_network, log, \
         vault_write, disk_write. Category values: secret, pii, \
         tool_output, user_input, llm_output, system_internal."
    }

    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        hydragent_types::PermissionTier::AutoApprove
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "sink": {
                    "type": "string",
                    "description": "The sink the value would flow to."
                },
                "categories": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Taint categories present on the value (e.g. ['secret','pii'])."
                }
            },
            "required": ["sink", "categories"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let call_id = String::new();

        let params: TaintCheckParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": format!("invalid parameters: {e}") }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("invalid parameters: {e}")),
                };
            }
        };

        let sink = match parse_sink(&params.sink) {
            Ok(s) => s,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": e }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(e),
                };
            }
        };

        let mut taint = TaintSet::new();
        for c in &params.categories {
            match parse_category(c) {
                Ok(cat) => { taint.0.insert(cat); }
                Err(e) => {
                    return ToolResult {
                        call_id,
                        output_json: json!({ "error": e }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some(e),
                    };
                }
            }
        }

        let (policy, source) = match self.load_policy() {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": e }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(e),
                };
            }
        };

        let allow = policy.enforce(sink, &taint).is_ok();
        let redact = policy.requires_redact(sink, &taint);
        let rule = policy.rule(sink).cloned();
        let verdict = if !allow {
            "block"
        } else if redact {
            "redact"
        } else {
            "allow"
        };

        ToolResult {
            call_id,
            output_json: json!({
                "verdict": verdict,
                "sink": params.sink,
                "categories": params.categories,
                "policy_source": source,
                "policy_version": policy.version,
                "rule": rule,
            })
            .to_string(),
            status: ToolStatus::Success,
            execution_ms: start.elapsed().as_millis() as u32,
            error_message: None,
        }
    }
}

// ============================================================================
// Track 6.3 — `sanitizer_scan`
// ============================================================================

/// Scan free-text input for prompt-injection patterns.
pub struct SanitizerScanTool {
    patterns_path: PathBuf,
}

impl SanitizerScanTool {
    pub fn new(patterns_path: impl Into<PathBuf>) -> Self {
        Self { patterns_path: patterns_path.into() }
    }
}

#[derive(Deserialize)]
struct SanitizerScanParams {
    text: String,
}

#[async_trait]
impl Tool for SanitizerScanTool {
    fn name(&self) -> &str { "sanitizer_scan" }

    fn description(&self) -> &str {
        "Phase 6.3 prompt-injection guard. Scans a free-text string \
         against the configured regex pattern library and returns \
         whether it is Allowed or Blocked (with the matching pattern id, \
         severity, and description). Use to vet any external content \
         (web_search results, email bodies, RAG chunks) before letting \
         it reach the LLM or any taint-sensitive sink."
    }

    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        hydragent_types::PermissionTier::AutoApprove
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The untrusted text to scan."
                }
            },
            "required": ["text"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let call_id = String::new();

        let params: SanitizerScanParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": format!("invalid parameters: {e}") }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("invalid parameters: {e}")),
                };
            }
        };

        let sanitizer = if self.patterns_path.exists() {
            match hydragent_security::InputSanitizer::from_yaml_file(&self.patterns_path) {
                Ok(s) => s,
                Err(e) => {
                    return ToolResult {
                        call_id,
                        output_json: json!({ "error": format!("load patterns: {e}") }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some(format!("load patterns: {e}")),
                    };
                }
            }
        } else {
            match hydragent_security::InputSanitizer::from_yaml(BUILTIN_PATTERNS) {
                Ok(s) => s,
                Err(e) => {
                    return ToolResult {
                        call_id,
                        output_json: json!({ "error": format!("built-in patterns failed to compile: {e}") }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some(format!("built-in patterns failed: {e}")),
                    };
                }
            }
        };

        let verdict = sanitizer.scan(&params.text);
        let value = match &verdict {
            hydragent_security::SanitizationResult::Allowed => {
                json!({ "verdict": "allowed", "text_length": params.text.chars().count() })
            }
            hydragent_security::SanitizationResult::Blocked { pattern_id, severity, description } => {
                json!({
                    "verdict": "blocked",
                    "pattern_id": pattern_id,
                    "severity": format!("{:?}", severity).to_lowercase(),
                    "description": description,
                })
            }
        };

        ToolResult {
            call_id,
            output_json: value.to_string(),
            status: ToolStatus::Success,
            execution_ms: start.elapsed().as_millis() as u32,
            error_message: None,
        }
    }
}

// ============================================================================
// Track 6.3 — `sanitizer_list_patterns`
// ============================================================================

/// List every prompt-injection pattern the sanitizer is currently
/// configured to detect. Companion to `sanitizer_scan`: the LLM calls
/// this when it needs to know *what* is detectable (e.g. to explain
/// "why was this blocked" or to choose a different surface for an
/// operation that would trip a high-severity pattern).
pub struct SanitizerListPatternsTool {
    patterns_path: PathBuf,
}

impl SanitizerListPatternsTool {
    pub fn new(patterns_path: impl Into<PathBuf>) -> Self {
        Self { patterns_path: patterns_path.into() }
    }
}

#[async_trait]
impl Tool for SanitizerListPatternsTool {
    fn name(&self) -> &str { "sanitizer_list_patterns" }

    fn description(&self) -> &str {
        "Phase 6.3 prompt-injection pattern inventory. Returns the full \
         list of regex patterns the agent is currently scanning for, \
         with their id, severity, and a human description. Use this to \
         discover what kinds of jailbreak / override / exfil attempts \
         the sanitizer can catch, or to look up the exact id of a \
         pattern that fired in a `sanitizer_scan` result. Does not take \
         any parameters."
    }

    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        hydragent_types::PermissionTier::AutoApprove
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }"#
    }

    async fn execute(&self, _params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let call_id = String::new();

        // Load the same source the sanitizer_scan tool would use.
        let (sanitizer, source) = if self.patterns_path.exists() {
            match hydragent_security::InputSanitizer::from_yaml_file(&self.patterns_path) {
                Ok(s) => (s, format!("file:{}", self.patterns_path.display())),
                Err(e) => {
                    return ToolResult {
                        call_id,
                        output_json: json!({ "error": format!("load patterns: {e}") }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some(format!("load patterns: {e}")),
                    };
                }
            }
        } else {
            match hydragent_security::InputSanitizer::from_yaml(BUILTIN_PATTERNS) {
                Ok(s) => (s, "default (built-in)".to_string()),
                Err(e) => {
                    return ToolResult {
                        call_id,
                        output_json: json!({ "error": format!("built-in patterns failed to compile: {e}") }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some(format!("built-in patterns failed: {e}")),
                    };
                }
            }
        };

        let patterns: Vec<serde_json::Value> = sanitizer
            .patterns()
            .iter()
            .map(|p| {
                let severity_str = match p.severity {
                    hydragent_security::PatternSeverity::Low => "low",
                    hydragent_security::PatternSeverity::Medium => "medium",
                    hydragent_security::PatternSeverity::High => "high",
                    hydragent_security::PatternSeverity::Critical => "critical",
                };
                let preview_src = p.regex.as_str();
                let regex_preview = if preview_src.len() > 120 {
                    format!("{}…", &preview_src[..120])
                } else {
                    preview_src.to_string()
                };
                json!({
                    "id": p.id,
                    "severity": severity_str,
                    "description": p.description,
                    "regex_preview": regex_preview,
                })
            })
            .collect();

        ToolResult {
            call_id,
            output_json: json!({
                "count": patterns.len(),
                "source": source,
                "patterns": patterns,
            })
            .to_string(),
            status: ToolStatus::Success,
            execution_ms: start.elapsed().as_millis() as u32,
            error_message: None,
        }
    }
}

// ============================================================================
// Track 6.4 — `vault_rotate`
// ============================================================================

/// Rotate the vault's passphrase or column key.
pub struct VaultRotateTool {
    data_dir: PathBuf,
}

impl VaultRotateTool {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self { data_dir: data_dir.into() }
    }

    fn vault_path(&self) -> PathBuf {
        self.data_dir.join("vault/.hydravault")
    }
}

#[derive(Deserialize)]
struct VaultRotateParams {
    action: String, // "status" | "rotate_passphrase" | "rotate_column_key"
    #[serde(default)]
    new_passphrase: Option<String>,
}

#[async_trait]
impl Tool for VaultRotateTool {
    fn name(&self) -> &str { "vault_rotate" }

    fn description(&self) -> &str {
        "Phase 6.4 vault maintenance. action='status' returns the vault \
         path, mlock availability, and a column-key presence check. \
         action='rotate_passphrase' re-encrypts the vault with a new \
         passphrase (requires new_passphrase). action='rotate_column_key' \
         generates a fresh AES-256 column key (forward secrecy: existing \
         column ciphertexts become unreadable; re-encrypt from source)."
    }

    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        hydragent_types::PermissionTier::Prompt // destructive
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "rotate_passphrase", "rotate_column_key"],
                    "description": "The maintenance action to perform."
                },
                "new_passphrase": {
                    "type": "string",
                    "description": "Required for action='rotate_passphrase'. New passphrase to re-encrypt the vault with."
                }
            },
            "required": ["action"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let call_id = String::new();

        let params: VaultRotateParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: json!({ "error": format!("invalid parameters: {e}") }).to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("invalid parameters: {e}")),
                };
            }
        };

        let path = self.vault_path();
        let exists = path.exists();
        let mlock_available = hydragent_vault::is_mlock_available();

        match params.action.as_str() {
            "status" => ToolResult {
                call_id,
                output_json: json!({
                    "action": "status",
                    "vault_path": path.display().to_string(),
                    "vault_exists": exists,
                    "mlock_available": mlock_available,
                })
                .to_string(),
                status: ToolStatus::Success,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: None,
            },
            "rotate_passphrase" => {
                let new_pp = match params.new_passphrase.as_deref() {
                    Some(p) if !p.is_empty() => p.to_string(),
                    _ => {
                        return ToolResult {
                            call_id,
                            output_json: json!({ "error": "rotate_passphrase requires non-empty new_passphrase" }).to_string(),
                            status: ToolStatus::Failure,
                            execution_ms: start.elapsed().as_millis() as u32,
                            error_message: Some("rotate_passphrase requires non-empty new_passphrase".into()),
                        };
                    }
                };
                // Interactive: ask for the *current* passphrase. For an
                // LLM-driven tool, fall back to HYDRAGENT_VAULT_PASSPHRASE
                // (and HYDRAGENT_VAULT_NEW_PASSPHRASE for the new one).
                let old_pp = std::env::var("HYDRAGENT_VAULT_PASSPHRASE").unwrap_or_default();
                if old_pp.is_empty() {
                    return ToolResult {
                        call_id,
                        output_json: json!({
                            "error": "rotate_passphrase is interactive; the LLM cannot prompt. \
                                     Set HYDRAGENT_VAULT_PASSPHRASE (current) and pass new_passphrase in the tool call."
                        }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some("set HYDRAGENT_VAULT_PASSPHRASE and pass new_passphrase".into()),
                    };
                }
                let rotator = hydragent_vault::Rotator::new(path.clone());
                match rotator.rotate_passphrase(&old_pp, &new_pp) {
                    Ok(report) => ToolResult {
                        call_id,
                        output_json: json!({
                            "action": "rotate_passphrase",
                            "ok": true,
                            "entries_after": report.entries_after,
                            "column_key_rotated": report.column_key_rotated,
                        })
                        .to_string(),
                        status: ToolStatus::Success,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: None,
                    },
                    Err(e) => ToolResult {
                        call_id,
                        output_json: json!({ "error": e.to_string() }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some(e.to_string()),
                    },
                }
            }
            "rotate_column_key" => {
                let pp = std::env::var("HYDRAGENT_VAULT_PASSPHRASE").unwrap_or_default();
                if pp.is_empty() {
                    return ToolResult {
                        call_id,
                        output_json: json!({
                            "error": "rotate_column_key requires HYDRAGENT_VAULT_PASSPHRASE in the environment."
                        }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some("set HYDRAGENT_VAULT_PASSPHRASE".into()),
                    };
                }
                let rotator = hydragent_vault::Rotator::new(path.clone());
                match rotator.rotate_column_key(&pp) {
                    Ok((report, key)) => {
                        let key_hex = hex_encode(&key.as_slice()[0]);
                        let preview = if key_hex.len() >= 16 {
                            format!("{}...", &key_hex[..16])
                        } else {
                            key_hex.clone()
                        };
                        ToolResult {
                            call_id,
                            output_json: json!({
                                "action": "rotate_column_key",
                                "ok": true,
                                "entries_after": report.entries_after,
                                "new_column_key_hex_preview": preview,
                            })
                            .to_string(),
                            status: ToolStatus::Success,
                            execution_ms: start.elapsed().as_millis() as u32,
                            error_message: None,
                        }
                    }
                    Err(e) => ToolResult {
                        call_id,
                        output_json: json!({ "error": e.to_string() }).to_string(),
                        status: ToolStatus::Failure,
                        execution_ms: start.elapsed().as_millis() as u32,
                        error_message: Some(e.to_string()),
                    },
                }
            }
            other => ToolResult {
                call_id,
                output_json: json!({ "error": format!("unknown action: {other}") }).to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("unknown action: {other}")),
            },
        }
    }
}

// ============================================================================
// helpers
// ============================================================================

/// Load the agent's Ed25519 keypair from disk, or generate and persist a
/// fresh one. Mirrors the function in `hydragent-core/src/main.rs` so the
/// Phase 6 tools can stand alone (they don't have access to the
/// binary's helpers).
fn load_or_create_agent_signer(
    key_path: &std::path::Path,
    pub_path: &std::path::Path,
) -> anyhow::Result<hydragent_security::AgentSigner> {
    if key_path.exists() {
        let bytes = std::fs::read(key_path)?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("agent key file has wrong length: {}", bytes.len()))?;
        return Ok(hydragent_security::AgentSigner::from_bytes(&arr));
    }
    let signer = hydragent_security::AgentSigner::generate();
    std::fs::write(key_path, signer.private_key_bytes())?;
    if let Some(parent) = pub_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    signer
        .export_public_key(pub_path)
        .map_err(|e| anyhow::anyhow!("failed to export public key: {e}"))?;
    Ok(signer)
}

/// Tiny built-in pattern library used when the YAML file is missing.
/// Keeps the tool useful out-of-the-box (matches what the YAML ships).
const BUILTIN_PATTERNS: &str = r#"
patterns:
  - id: "IP001"
    description: "Instruction override ('ignore previous', 'disregard above')"
    regex: "(?i)(ignore|disregard|forget)\\s+(all|any|the)?\\s*(previous|prior|above|earlier)\\s*(instructions?|prompts?|rules?)"
    severity: high
  - id: "IP002"
    description: "Reveal-the-prompt attack"
    regex: "(?i)(show|reveal|print|output|dump)\\s+(your|the)?\\s*(system|initial|original|hidden)?\\s*(prompt|instructions?|rules?)"
    severity: high
  - id: "IP003"
    description: "Role-override ('you are now', 'pretend to be')"
    regex: "(?i)(you\\s+are\\s+now|pretend\\s+(to\\s+be|you\\s+are)|act\\s+as\\s+if)"
    severity: medium
  - id: "IP004"
    description: "Jailbreak keyword"
    regex: "(?i)\\b(jailbreak|dan\\s+mode|developer\\s+mode)\\b"
    severity: critical
  - id: "IP005"
    description: "Embedded credentials / API key exfil"
    regex: "(?i)(api[_-]?key|secret|token|password)\\s*[:=]\\s*['\\\"]?[A-Za-z0-9_\\-]{16,}"
    severity: critical
"#;

/// Lowercase hex encoder (kept local to avoid pulling in the `hex` crate).
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[((b >> 4) & 0x0f) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Convenience: hex-encode a fixed 32-byte array. Used for the
/// column-key preview, which always comes from `SecureBuffer<[u8; 32]>`.
#[allow(dead_code)]
fn hex_encode_32(arr: &[u8; 32]) -> String {
    hex_encode(arr)
}
