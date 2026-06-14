// crates/hydragent-types/src/lib.rs
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// Inbound user message, normalised from any channel adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentEvent {
    /// UUID v4 — uniquely identifies the page
    pub page_id: String,
    /// e.g. "cli:default", "telegram:123456789"
    pub channel_id: String,
    pub user_id: String,
    /// Raw message text
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// Unix epoch milliseconds
    pub timestamp: i64,
    pub priority: Priority,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Urgent,
    #[default]
    Normal,
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub mime_type: String,
    /// Local file path or base64 data URI
    pub data: String,
    pub filename: Option<String>,
}

/// Agent response returned to the channel adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub page_id: String,
    pub content: String,
    pub format: ResponseFormat,
    #[serde(default)]
    pub consent_requests: Vec<ConsentRequest>,  // Phase 3+
    #[serde(default)]
    pub tool_calls_executed: Vec<ToolCallRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    #[default]
    Markdown,
    Plain,
    Html,
}

/// A request to invoke a registered tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,           // UUID v4
    pub tool_id: String,           // e.g. "web_search"
    pub params_json: String,       // JSON-encoded params (NO raw credentials)
    pub tier: PermissionTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionTier {
    #[default]
    AutoApprove,
    Prompt,
    Deny,
}

/// Result returned by a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output_json: String,
    pub status: ToolStatus,
    pub execution_ms: u32,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Success,
    Failure,
    Timeout,
}

/// Stored in SQLite for audit display (credentials never stored here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub call_id: String,
    pub tool_id: String,
    pub params_hash: String,    // SHA-256 of params
    pub status: ToolStatus,
    pub execution_ms: u32,
    pub timestamp: i64,
}

/// Consent request sent to user before Prompt-tier tool executes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRequest {
    pub call_id: String,
    pub tool_id: String,
    pub description: String,
    pub tier: PermissionTier,
}

/// A single conversation turn stored in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Message {
    pub id: i64,
    pub page_id: String,
    pub role: MessageRole,
    pub content: String,
    pub timestamp: i64,
    pub token_count: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

/// Active reasoning state for one conversation turn.
#[derive(Debug, Clone)]
pub struct ReActContext {
    pub intent: IntentEvent,
    pub history: Vec<Message>,
    pub current_step: u8,
    pub max_steps: u8,
    pub tool_results: Vec<ToolResult>,
    pub final_answer: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_intent_event_serialization() {
        let mut metadata = HashMap::new();
        metadata.insert("key".to_string(), "value".to_string());
        
        let event = IntentEvent {
            page_id: "session-123".to_string(),
            channel_id: "cli:default".to_string(),
            user_id: "user-456".to_string(),
            content: "hello".to_string(),
            attachments: vec![Attachment {
                mime_type: "text/plain".to_string(),
                data: "base64data".to_string(),
                filename: Some("test.txt".to_string()),
            }],
            metadata,
            timestamp: 1620000000000,
            priority: Priority::Normal,
        };

        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: IntentEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.page_id, "session-123");
        assert_eq!(deserialized.content, "hello");
    }

    #[test]
    fn test_agent_response_serialization() {
        let response = AgentResponse {
            page_id: "session-123".to_string(),
            content: "response content".to_string(),
            format: ResponseFormat::Markdown,
            consent_requests: vec![],
            tool_calls_executed: vec![],
        };

        let serialized = serde_json::to_string(&response).unwrap();
        let deserialized: AgentResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.page_id, "session-123");
        assert_eq!(deserialized.content, "response content");
    }

    #[test]
    fn test_tool_call_serialization() {
        let call = ToolCall {
            call_id: "call-123".to_string(),
            tool_id: "web_search".to_string(),
            params_json: r#"{"query":"rust"}"#.to_string(),
            tier: PermissionTier::AutoApprove,
        };

        let serialized = serde_json::to_string(&call).unwrap();
        let deserialized: ToolCall = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.call_id, "call-123");
        assert_eq!(deserialized.tool_id, "web_search");
    }

    #[test]
    fn test_tool_result_serialization() {
        let result = ToolResult {
            call_id: "call-123".to_string(),
            output_json: r#"{"result":"ok"}"#.to_string(),
            status: ToolStatus::Success,
            execution_ms: 120,
            error_message: None,
        };

        let serialized = serde_json::to_string(&result).unwrap();
        let deserialized: ToolResult = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.call_id, "call-123");
        assert_eq!(deserialized.execution_ms, 120);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDocument {
    pub id: String,
    pub content: String,
    pub timestamp: i64,
    pub importance: i64,
    pub rrf_score: f64,
}

/// Emitted by the orchestrator on the bus when a `Prompt` tier action is requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub request_id: String,     // UUID
    pub page_id: String,
    pub tool_id: String,
    pub params_summary: String, // Human-readable description of the action
    pub tier: PermissionTier,
    pub expires_at_ms: i64,     // Timestamp after which auto-deny triggers
}

/// Response from the channel adapter (user decision).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub request_id: String,
    pub approved: bool,
}

/// What a channel can do — not all adapters support all capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelCapabilities {
    /// Can deliver token-by-token streaming (e.g., edit-in-place)
    pub streaming: bool,
    /// Can receive file/image attachments
    pub file_attachments: bool,
    /// Can render Markdown formatting
    pub markdown: bool,
    /// Supports buttons/interactive elements
    pub interactive: bool,
    /// Maximum message length in characters (platform-enforced)
    pub max_message_len: usize,
}

/// A proactive message pushed from agent to user (agent-initiated, not response to query).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushMessage {
    pub channel_id: String,       // e.g., "telegram:123456789"
    pub page_id: String,
    pub content: String,
    pub markdown: bool,
    pub metadata: HashMap<String, String>,
}/// A scheduled cron job stored in SQLite and registered with the scheduler.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct CronJob {
    pub id: String,               // UUID
    pub cron_expr: String,        // e.g., "0 9 * * *"
    pub description: String,      // Human-readable description
    pub task_type: String,        // "react_loop" | "heartbeat" | "work_iq_digest"
    pub task_params: String,      // JSON params for the task
    pub target_channel_id: String,// Where to deliver results
    pub status: String,           // "active" | "paused" | "deleted"
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub run_count: i64,
}

// ============================================================================
// Sub-agent swarm types (Phase 5 / Track 5.1)
// ============================================================================

// ============================================================================
// Sub-agent swarm types (Phase 5 / Track 5.1)
// ============================================================================

/// A clarification question the orchestrator is waiting on the user to
/// answer. The `IntentSubmitHandler` keeps one of these per `page_id` —
/// when a new `intent.submit` arrives on the same page, the pending
/// question is popped, the new user content is treated as the answer,
/// and the original query is re-run with the answer included.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingClarification {
    pub page_id: String,
    pub question: String,
    /// Unix epoch milliseconds.
    pub asked_at_ms: i64,
    /// Strategy source for logging/debugging (e.g. "llm", "heuristic").
    pub source: String,
}

/// Specialist role for a sub-agent. Determines default system prompt + tool bias.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentRole {
    /// Plans, decomposes, orchestrates
    Plan,
    /// Writes code, runs build/tests
    Build,
    /// Researches, searches, reads
    Explore,
    /// Fast, cheap reconnaissance
    Scout,
    /// Reviews outputs, finds issues
    Review,
    /// Generic catch-all
    General,
}

impl SubAgentRole {
    /// Default tools each role is allowed to use.
    pub fn default_tools(&self) -> &'static [&'static str] {
        match self {
            SubAgentRole::Plan     => &["memory_search", "memory_store"],
            SubAgentRole::Build    => &["file_read", "echo"],
            SubAgentRole::Explore  => &["web_search", "memory_search", "memory_store", "file_read"],
            SubAgentRole::Scout    => &["web_search"],
            SubAgentRole::Review   => &["file_read", "memory_search"],
            SubAgentRole::General  => &["echo"],
        }
    }

    /// Default token budget per role.
    pub fn default_token_budget(&self) -> u32 {
        match self {
            SubAgentRole::Plan     => 2_000,
            SubAgentRole::Build    => 4_000,
            SubAgentRole::Explore  => 3_000,
            SubAgentRole::Scout    => 1_500,
            SubAgentRole::Review   => 2_500,
            SubAgentRole::General  => 1_500,
        }
    }

    /// Default timeout in milliseconds per role.
    pub fn default_timeout_ms(&self) -> u64 {
        match self {
            SubAgentRole::Plan     => 30_000,
            SubAgentRole::Build    => 60_000,
            SubAgentRole::Explore  => 45_000,
            SubAgentRole::Scout    => 20_000,
            SubAgentRole::Review   => 30_000,
            SubAgentRole::General  => 20_000,
        }
    }
}

/// Specification for spawning a sub-agent. Produced by decomposers, builders, or hand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentSpec {
    /// UUID. Generated by the caller if not provided.
    pub id: String,
    /// Human label (e.g., "Research Actix-Web")
    pub name: String,
    /// Specialist role (influences defaults)
    pub role: SubAgentRole,
    /// System prompt — if empty, the role's default is used
    pub system_prompt: String,
    /// The actual task to execute (user query that goes into the LLM call)
    pub task: String,
    /// Whitelist of tool names this sub-agent may invoke.
    /// Enforced in `SubAgent` before delegating to the shared `ToolRegistry`.
    pub allowed_tools: Vec<String>,
    /// Hint for which model to use (None = router picks).
    /// Currently informational only; router is consulted downstream.
    pub model_hint: Option<String>,
    /// Hard cap on tokens (informational; LLM client may also enforce).
    pub token_budget: u32,
    /// Hard cap on wall-clock duration.
    pub timeout_ms: u64,
    /// Logical grouping (for trace correlation + cleanup).
    pub swarm_id: String,
    /// Page the agent reports to (e.g., the originating user page).
    pub parent_page_id: String,
}

impl SubAgentSpec {
    /// Quick builder with role-derived defaults.
    pub fn new(name: impl Into<String>, role: SubAgentRole, task: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            role,
            system_prompt: String::new(),
            task: task.into(),
            allowed_tools: role.default_tools().iter().map(|s| s.to_string()).collect(),
            model_hint: None,
            token_budget: role.default_token_budget(),
            timeout_ms: role.default_timeout_ms(),
            swarm_id: String::new(),
            parent_page_id: String::new(),
        }
    }

    /// Override the tool allowlist.
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    /// Override the token budget.
    pub fn with_token_budget(mut self, n: u32) -> Self {
        self.token_budget = n;
        self
    }

    /// Override the timeout.
    pub fn with_timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    /// Attach to a swarm + parent page.
    pub fn in_swarm(mut self, swarm_id: impl Into<String>, parent_page_id: impl Into<String>) -> Self {
        self.swarm_id = swarm_id.into();
        self.parent_page_id = parent_page_id.into();
        self
    }
}

/// Lifecycle state of a sub-agent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Queued, not yet running
    Pending,
    /// Currently executing
    Running,
    /// Finished successfully
    Completed,
    /// Finished with error
    Failed,
    /// Killed by timeout / budget / external cancel
    Cancelled,
}

impl AgentState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, AgentState::Completed | AgentState::Failed | AgentState::Cancelled)
    }
}

// ============================================================================
// Phase 6 / Track 6.2: Taint tracking primitives
// ============================================================================
//
// The Phase 3 vault taint types (`hydragent_vault::taint::TaintCategory`)
// remain in place for backwards compatibility. The new types below
// follow the Phase 6 specification: 6 categories, BTreeSet-backed
// sets for deterministic display, and a generic `TaintedValue<T>`
// wrapper that works for any payload (not just strings).
//
// Mapping from old → new lives in `hydragent-security::taint` (not
// here) so the types crate stays free of any cross-crate coupling.

/// Phase 6 spec-compliant taint categories.
///
/// The 6 categories partition the data-flow surface so the security
/// layer can apply category-specific policies. The categories are
/// **comparable** (Ord) so [`TaintSet`] can use a `BTreeSet` for
/// deterministic ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaintCategory {
    /// Vault credential, API key, token, password. Most restrictive —
    /// cannot leave the host and cannot be logged.
    Secret,
    /// Personally identifiable information (name, email, SSN, phone).
    /// `lowercase` correctly lowercases acronyms (`PII` → `pii`),
    /// unlike `snake_case` which would emit `p_i_i`.
    PII,
    /// Output produced by a tool (web_search, file_read, etc.).
    #[serde(rename = "tool_output")]
    ToolOutput,
    /// Raw user input as received from the channel. Distinct from
    /// `ToolOutput` so policies can keep user prompts un-redacted in
    /// audit logs while still requiring scrubbing of tool data.
    #[serde(rename = "user_input")]
    UserInput,
    /// Output produced by the LLM (assistant content, completions).
    /// Cannot be used as a shell command or sent to outbound network
    /// unless explicitly cleared.
    #[serde(rename = "llm_output")]
    LlmOutput,
    /// System-authored metadata (timestamps, IDs, internal state).
    #[serde(rename = "system_internal")]
    SystemInternal,
}

impl TaintCategory {
    /// Snake-case form (matches `#[serde(rename_all = "snake_case")]`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Secret         => "secret",
            Self::PII            => "pii",
            Self::ToolOutput     => "tool_output",
            Self::UserInput      => "user_input",
            Self::LlmOutput      => "llm_output",
            Self::SystemInternal => "system_internal",
        }
    }

    /// All 6 categories, in canonical order.
    pub const ALL: [TaintCategory; 6] = [
        TaintCategory::Secret,
        TaintCategory::PII,
        TaintCategory::ToolOutput,
        TaintCategory::UserInput,
        TaintCategory::LlmOutput,
        TaintCategory::SystemInternal,
    ];
}

impl std::fmt::Display for TaintCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A set of [`TaintCategory`] values. `BTreeSet`-backed for
/// deterministic iteration and JSON serialization.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaintSet(pub BTreeSet<TaintCategory>);

impl TaintSet {
    pub fn new() -> Self { Self(BTreeSet::new()) }

    pub fn singleton(c: TaintCategory) -> Self {
        let mut s = BTreeSet::new();
        s.insert(c);
        Self(s)
    }

    pub fn from_iter(cats: impl IntoIterator<Item = TaintCategory>) -> Self {
        Self(cats.into_iter().collect())
    }

    pub fn insert(&mut self, c: TaintCategory) -> bool { self.0.insert(c) }

    pub fn contains(&self, c: TaintCategory) -> bool { self.0.contains(&c) }

    pub fn iter(&self) -> impl Iterator<Item = &TaintCategory> { self.0.iter() }

    pub fn is_empty(&self) -> bool { self.0.is_empty() }

    pub fn len(&self) -> usize { self.0.len() }

    pub fn as_inner(&self) -> &BTreeSet<TaintCategory> { &self.0 }

    pub fn union(&self, other: &TaintSet) -> TaintSet {
        Self(self.0.union(&other.0).cloned().collect())
    }

    pub fn intersection(&self, other: &TaintSet) -> TaintSet {
        Self(self.0.intersection(&other.0).cloned().collect())
    }

    pub fn difference(&self, other: &TaintSet) -> TaintSet {
        Self(self.0.difference(&other.0).cloned().collect())
    }
}

impl FromIterator<TaintCategory> for TaintSet {
    fn from_iter<I: IntoIterator<Item = TaintCategory>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl From<TaintCategory> for TaintSet {
    fn from(c: TaintCategory) -> Self { Self::singleton(c) }
}

impl std::fmt::Display for TaintSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return write!(f, "(trusted)");
        }
        let parts: Vec<String> = self.0.iter().map(|c| c.to_string()).collect();
        write!(f, "[{}]", parts.join(","))
    }
}

/// Generic wrapper that attaches [`TaintSet`] metadata to any value
/// `T`. Use this for non-string values (paths, JSON blobs, byte
/// buffers, …). For strings, prefer
/// [`hydragent_vault::taint::TaintedString`] which has built-in
/// redaction and `Zeroize` on drop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintedValue<T> {
    pub value: T,
    pub taint: TaintSet,
}

impl<T> TaintedValue<T> {
    /// Create a *trusted* (untagged) value. Use for system-authored data.
    pub fn trusted(value: T) -> Self {
        Self { value, taint: TaintSet::new() }
    }

    /// Create a value tagged with a single category.
    pub fn with_taint(value: T, category: TaintCategory) -> Self {
        Self { value, taint: TaintSet::singleton(category) }
    }

    /// Create a value tagged with an arbitrary set.
    pub fn with_taints(value: T, taint: TaintSet) -> Self {
        Self { value, taint }
    }

    pub fn is_tainted(&self) -> bool { !self.taint.is_empty() }

    pub fn has_taint(&self, c: TaintCategory) -> bool { self.taint.contains(c) }

    /// Propagate taint through a transformation. Result is tainted
    /// with the union of input taint plus any extra taint the
    /// transformation introduced (e.g., converting a `UserInput`
    /// string to a `LlmOutput` keeps the input tag and adds
    /// `LlmOutput`).
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F, extra_taint: TaintSet) -> TaintedValue<U> {
        let combined = self.taint.union(&extra_taint);
        TaintedValue { value: f(self.value), taint: combined }
    }

    /// Combine two tainted values into one. Result is tainted with
    /// the union of both inputs' taint.
    pub fn merge(
        self,
        other: TaintedValue<T>,
        combiner: impl FnOnce(T, T) -> T,
    ) -> TaintedValue<T> {
        let combined = self.taint.union(&other.taint);
        TaintedValue { value: combiner(self.value, other.value), taint: combined }
    }

    /// Strip all taint, asserting the value has been validated.
    pub fn into_sanitized(self) -> TaintedValue<T> {
        TaintedValue { value: self.value, taint: TaintSet::new() }
    }
}

#[cfg(test)]
mod taint_type_tests {
    use super::*;

    #[test]
    fn taint_category_serde_roundtrip() {
        for c in TaintCategory::ALL {
            let s = serde_json::to_string(&c).unwrap();
            let back: TaintCategory = serde_json::from_str(&s).unwrap();
            assert_eq!(c, back, "roundtrip failed for {c}");
        }
    }

    #[test]
    fn taint_category_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&TaintCategory::Secret).unwrap(),         "\"secret\"");
        assert_eq!(serde_json::to_string(&TaintCategory::PII).unwrap(),            "\"pii\"");
        assert_eq!(serde_json::to_string(&TaintCategory::ToolOutput).unwrap(),     "\"tool_output\"");
        assert_eq!(serde_json::to_string(&TaintCategory::UserInput).unwrap(),      "\"user_input\"");
        assert_eq!(serde_json::to_string(&TaintCategory::LlmOutput).unwrap(),      "\"llm_output\"");
        assert_eq!(serde_json::to_string(&TaintCategory::SystemInternal).unwrap(), "\"system_internal\"");
    }

    #[test]
    fn taint_category_as_str_matches_serde() {
        for c in TaintCategory::ALL {
            assert_eq!(c.as_str(), serde_json::to_string(&c).unwrap().trim_matches('"'));
        }
    }

    #[test]
    fn taint_set_empty_is_trusted() {
        let s = TaintSet::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.to_string(), "(trusted)");
    }

    #[test]
    fn taint_set_singleton_has_one() {
        let s = TaintSet::singleton(TaintCategory::Secret);
        assert_eq!(s.len(), 1);
        assert!(s.contains(TaintCategory::Secret));
    }

    #[test]
    fn taint_set_insert_dedupes() {
        let mut s = TaintSet::new();
        assert!(s.insert(TaintCategory::Secret));
        assert!(!s.insert(TaintCategory::Secret));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn taint_set_union_intersection_difference() {
        let a: TaintSet = [TaintCategory::Secret, TaintCategory::PII].into_iter().collect();
        let b: TaintSet = [TaintCategory::PII, TaintCategory::UserInput].into_iter().collect();
        let u = a.union(&b);
        assert_eq!(u.len(), 3);
        let i = a.intersection(&b);
        assert_eq!(i.len(), 1);
        assert!(i.contains(TaintCategory::PII));
        let d = a.difference(&b);
        assert_eq!(d.len(), 1);
        assert!(d.contains(TaintCategory::Secret));
    }

    #[test]
    fn taint_set_serde_order_is_deterministic() {
        // BTreeSet orders by Ord; pii < secret < tool_output alphabetically
        // (PII < Secret < ToolOutput because the enum variants are
        // declared in that order). We only assert *some* deterministic
        // order here, not a specific ordering.
        let mut s = TaintSet::new();
        s.insert(TaintCategory::ToolOutput);
        s.insert(TaintCategory::Secret);
        s.insert(TaintCategory::PII);
        let json1 = serde_json::to_string(&s).unwrap();
        // Run twice and compare
        let json2 = serde_json::to_string(&s).unwrap();
        assert_eq!(json1, json2);
    }

    #[test]
    fn taint_set_display_shows_all() {
        let s: TaintSet = [TaintCategory::Secret, TaintCategory::PII].into_iter().collect();
        let displayed = s.to_string();
        assert!(displayed.contains("secret"));
        assert!(displayed.contains("pii"));
        assert!(displayed.starts_with('['));
        assert!(displayed.ends_with(']'));
    }

    #[test]
    fn tainted_value_trusted_is_clean() {
        let v: TaintedValue<String> = TaintedValue::trusted("hello".into());
        assert!(!v.is_tainted());
        assert_eq!(v.value, "hello");
    }

    #[test]
    fn tainted_value_with_taint() {
        let v = TaintedValue::with_taint("ghp_x".to_string(), TaintCategory::Secret);
        assert!(v.is_tainted());
        assert!(v.has_taint(TaintCategory::Secret));
        assert!(!v.has_taint(TaintCategory::PII));
    }

    #[test]
    fn tainted_value_map_propagates_taint() {
        let v = TaintedValue::with_taint("hello".to_string(), TaintCategory::UserInput);
        let mapped = v.map(
            |s| s.to_uppercase(),
            TaintSet::singleton(TaintCategory::LlmOutput),
        );
        assert!(mapped.has_taint(TaintCategory::UserInput));
        assert!(mapped.has_taint(TaintCategory::LlmOutput));
        assert_eq!(mapped.value, "HELLO");
    }

    #[test]
    fn tainted_value_merge_unions_taint() {
        let a = TaintedValue::with_taint("a".to_string(), TaintCategory::UserInput);
        let b = TaintedValue::with_taint("b".to_string(), TaintCategory::ToolOutput);
        let m = a.merge(b, |x, y| format!("{x}{y}"));
        assert!(m.has_taint(TaintCategory::UserInput));
        assert!(m.has_taint(TaintCategory::ToolOutput));
        assert_eq!(m.value, "ab");
    }

    #[test]
    fn tainted_value_sanitize_clears() {
        let v = TaintedValue::with_taint("secret".to_string(), TaintCategory::Secret);
        let clean = v.into_sanitized();
        assert!(!clean.is_tainted());
        assert_eq!(clean.value, "secret");
    }
}

/// Final report from a finished sub-agent. Emitted on the spawner's result channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentStatus {
    pub id: String,
    pub name: String,
    pub role: SubAgentRole,
    pub swarm_id: String,
    pub parent_page_id: String,
    pub state: AgentState,
    pub model_used: String,
    pub tokens_used: u32,
    pub elapsed_ms: u64,
    /// Final text answer (or last assistant message) on success.
    pub output: String,
    /// Tool calls executed during the run, in order.
    pub tool_calls: Vec<ToolCall>,
    /// Error message if state == Failed or Cancelled.
    pub error: Option<String>,
}

// ============================================================================
// Phase 6: Security / Merkle audit types
// ============================================================================

/// Discriminator for [`AuditEvent`] entries appended to the Merkle chain.
///
/// Serialised as snake_case in the SQLite `event_type` column so external
/// verifiers can match events without depending on Rust's `Debug` output.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// Inbound event accepted by the security pipeline
    Inbound,
    /// Outbound agent response delivered
    Outbound,
    /// Tool invocation started
    ToolCall,
    /// Tool invocation completed
    ToolCallComplete,
    /// Vault credential accessed (read or write)
    VaultAccess,
    /// Prompt-injection attempt blocked by the sanitizer
    InjectionBlocked,
    /// SGNL continuous-auth decision emitted
    AuthDecision,
    /// Session risk score updated
    RiskUpdate,
    /// Taint violation blocked at a sink boundary
    TaintViolation,
    /// Agent response Ed25519-signed before delivery
    ResponseSigned,
    /// Agent boot / vault init
    AgentBoot,
    /// Catch-all for ad-hoc events
    Other,
}

impl AuditEventType {
    /// Stable string form used as the SQLite `event_type` column value.
    pub fn as_str(&self) -> &'static str {
        match self {
            AuditEventType::Inbound           => "inbound",
            AuditEventType::Outbound          => "outbound",
            AuditEventType::ToolCall          => "tool_call",
            AuditEventType::ToolCallComplete  => "tool_call_complete",
            AuditEventType::VaultAccess       => "vault_access",
            AuditEventType::InjectionBlocked  => "injection_blocked",
            AuditEventType::AuthDecision      => "auth_decision",
            AuditEventType::RiskUpdate        => "risk_update",
            AuditEventType::TaintViolation    => "taint_violation",
            AuditEventType::ResponseSigned    => "response_signed",
            AuditEventType::AgentBoot         => "agent_boot",
            AuditEventType::Other             => "other",
        }
    }
}

/// A single audit record. Serialised canonically (sorted keys) and appended
/// to the Merkle chain by [`hydragent_security::MerkleAuditChain`].
///
/// Chain layout (per row):
///   `event_hash`     = SHA-256(serialize(event))
///   `chain_hash`     = SHA-256(prev_hash || event_hash)
///   `agent_signature`= Ed25519(signing_key, chain_hash)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub event_type: AuditEventType,
    /// Who/what triggered the event (e.g. `"channel:telegram:1234"`,
    /// `"user:alice"`, `"system"`).
    pub actor: String,
    /// Page this event belongs to. `None` for system-wide events.
    pub page_id: Option<String>,
    /// Free-form structured detail. Use [`serde_json::json!({})`] in callers;
    /// stored verbatim in the chain row's `event_json` column.
    pub detail: serde_json::Value,
    /// Unix epoch milliseconds.
    pub timestamp_ms: i64,
}

impl AuditEvent {
    /// Construct a new event with `timestamp_ms = now` (in millis).
    pub fn now(event_type: AuditEventType, actor: impl Into<String>) -> Self {
        Self {
            event_type,
            actor: actor.into(),
            page_id: None,
            detail: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        }
    }

    pub fn with_page(mut self, page_id: impl Into<String>) -> Self {
        self.page_id = Some(page_id.into());
        self
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = detail;
        self
    }
}

/// A [`ToolCallRecord`] paired with its Ed25519 signature (hex-encoded).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedToolCall {
    pub call: ToolCallRecord,
    /// Hex-encoded Ed25519 signature over the canonical JSON of `call`.
    pub signature: String,
}

/// An [`AgentResponse`] paired with its Ed25519 signature (hex-encoded).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedResponse {
    pub response: AgentResponse,
    /// Hex-encoded Ed25519 signature over the response content + page.
    pub signature: String,
}

/// Internal marker trait for the security crate; kept here so future tracks
/// can re-export the type without a circular dependency.
pub trait AuditRecord: Send + Sync {
    fn event_type(&self) -> AuditEventType;
    fn actor(&self) -> &str;
    fn page_id(&self) -> Option<&str>;
    fn timestamp_ms(&self) -> i64;
}

impl AuditRecord for AuditEvent {
    fn event_type(&self) -> AuditEventType { self.event_type }
    fn actor(&self) -> &str { &self.actor }
    fn page_id(&self) -> Option<&str> { self.page_id.as_deref() }
    fn timestamp_ms(&self) -> i64 { self.timestamp_ms }
}

#[cfg(test)]
mod audit_event_tests {
    use super::*;

    #[test]
    fn event_type_serializes_snake_case() {
        let s = serde_json::to_string(&AuditEventType::InjectionBlocked).unwrap();
        assert_eq!(s, "\"injection_blocked\"");

        let s = serde_json::to_string(&AuditEventType::ToolCallComplete).unwrap();
        assert_eq!(s, "\"tool_call_complete\"");

        // Round-trip
        let parsed: AuditEventType = serde_json::from_str("\"vault_access\"").unwrap();
        assert_eq!(parsed, AuditEventType::VaultAccess);
    }

    #[test]
    fn event_type_as_str_matches_serde() {
        for v in [
            AuditEventType::Inbound,
            AuditEventType::Outbound,
            AuditEventType::ToolCall,
            AuditEventType::ToolCallComplete,
            AuditEventType::VaultAccess,
            AuditEventType::InjectionBlocked,
            AuditEventType::AuthDecision,
            AuditEventType::RiskUpdate,
            AuditEventType::TaintViolation,
            AuditEventType::ResponseSigned,
            AuditEventType::AgentBoot,
            AuditEventType::Other,
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap().trim_matches('"'), v.as_str());
        }
    }

    #[test]
    fn audit_event_now_uses_current_timestamp() {
        let before = chrono::Utc::now().timestamp_millis();
        let e = AuditEvent::now(AuditEventType::AgentBoot, "test");
        let after = chrono::Utc::now().timestamp_millis();
        assert!(e.timestamp_ms >= before && e.timestamp_ms <= after);
        assert_eq!(e.actor, "test");
        assert!(e.page_id.is_none());
        assert_eq!(e.detail, serde_json::Value::Null);
    }

    #[test]
    fn audit_event_builder_chain() {
        let e = AuditEvent::now(AuditEventType::ToolCall, "user:alice")
            .with_page("page-123")
            .with_detail(serde_json::json!({"tool": "web_search", "q": "rust"}));
        assert_eq!(e.page_id.as_deref(), Some("page-123"));
        assert_eq!(e.detail["tool"], "web_search");
    }

    #[test]
    fn audit_event_serializes_to_json() {
        let e = AuditEvent::now(AuditEventType::InjectionBlocked, "channel:telegram:42")
            .with_page("page-xyz")
            .with_detail(serde_json::json!({"pattern_id": "IP001"}));
        let s = serde_json::to_string(&e).unwrap();
        let back: AuditEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.event_type, AuditEventType::InjectionBlocked);
        assert_eq!(back.actor, "channel:telegram:42");
        assert_eq!(back.page_id.as_deref(), Some("page-xyz"));
        assert_eq!(back.detail["pattern_id"], "IP001");
    }

    #[test]
    fn audit_record_trait_returns_expected_fields() {
        let e = AuditEvent::now(AuditEventType::VaultAccess, "user:bob")
            .with_page("page-7");
        assert_eq!(e.event_type(), AuditEventType::VaultAccess);
        assert_eq!(e.actor(), "user:bob");
        assert_eq!(e.page_id(), Some("page-7"));
    }
}

// ============================================================================
// Phase 7 / Track 7.1: Self-improving skill types
// ============================================================================
//
// A `Skill` is a re-usable prompt + param schema + tool allowlist that the
// agent can invoke via `SkillExecutor`. Skills are stored in the
// `skill_library` table, versioned in `skill_versions`, and indexed by
// FTS5 over (name, description) for retrieval.

/// Lifecycle tier of a skill.
///
/// Skills move `Candidate -> Active` once `success_rate >= 0.7` over at
/// least 10 executions (see `hydragent_skills::curator`). `Inactive` is
/// a manual off-switch (kept around but not eligible for retrieval).
/// `Archived` means the skill was retired by the curator and should
/// not be invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillTier {
    /// Just induced from a successful trajectory. Insufficient data
    /// to call it reliable. Surfaced in the prompt with a "candidate"
    /// tag and a lower priority.
    #[default]
    Candidate,
    /// Approved for normal retrieval. Default tier.
    Active,
    /// Manually disabled (e.g., broken or unsafe). Still in the
    /// library so its history is preserved.
    Inactive,
    /// Retired by the curator (low success rate, stale, or
    /// superseded). Excluded from retrieval.
    Archived,
}

impl SkillTier {
    /// Snake-case form used in SQLite `tier` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Active    => "active",
            Self::Inactive  => "inactive",
            Self::Archived  => "archived",
        }
    }

    /// Whether skills of this tier are eligible for retrieval.
    pub fn is_retrievable(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// All tiers, in promotion order.
    pub const ALL: [SkillTier; 4] = [
        SkillTier::Candidate,
        SkillTier::Active,
        SkillTier::Inactive,
        SkillTier::Archived,
    ];
}

impl std::fmt::Display for SkillTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SkillTier {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "candidate" => Ok(Self::Candidate),
            "active"    => Ok(Self::Active),
            "inactive"  => Ok(Self::Inactive),
            "archived"  => Ok(Self::Archived),
            other       => Err(format!("unknown SkillTier: {other:?}")),
        }
    }
}

/// One named input parameter for a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillParam {
    pub name: String,
    /// `"string"`, `"int"`, `"float"`, `"bool"`, `"json"`, `"path"`.
    #[serde(rename = "type")]
    pub type_: String,
    /// Human-readable description; rendered in the skill's `usage`
    /// block in the prompt.
    pub description: String,
    pub required: bool,
}

/// A re-usable prompt + param schema + tool allowlist.
///
/// One row in `skill_library`. The `prompt_template` uses standard
/// Mustache-style `{{param_name}}` placeholders that
/// `SkillExecutor::render` expands at execution time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    /// Stable identifier (UUID v4, generated at induction time).
    pub id: String,
    /// Human-readable kebab-case name (e.g. `"convert-csv-to-json"`).
    pub name: String,
    /// Monotonically increasing per-`id` version. Starts at 1.
    pub version: u32,
    /// One-line description used for retrieval ranking and prompt
    /// surfacing.
    pub description: String,
    pub tier: SkillTier,
    /// Free-form capability tags (e.g. `"text"`, `"code"`,
    /// `"github"`, `"csv"`). Indexed in the `skill_tags` table for
    /// tag-based retrieval.
    pub capability_tags: Vec<String>,
    /// Ordered list of accepted input parameters.
    pub params: Vec<SkillParam>,
    /// Mustache-style prompt template. `{{param_name}}` placeholders
    /// are replaced with the supplied param values at execution.
    pub prompt_template: String,
    /// Tool names the skill is allowed to invoke. Enforced by
    /// `SkillExecutor` before delegating to the `ToolRegistry`.
    pub required_tools: Vec<String>,
    /// Up to 5 example invocations that produced successful outputs.
    /// Used for in-context few-shot learning when the skill is
    /// surfaced to the LLM.
    pub success_examples: Vec<String>,
    /// Author identifier (`"user:alice"`, `"hermes:induction"`,
    /// `"builtin"`, etc.).
    pub author: String,
    /// Unix epoch milliseconds (skill first created).
    pub created_at: i64,
    /// Unix epoch milliseconds (last `update_skill` call).
    pub last_updated: i64,
    /// Rolling success rate, 0.0-1.0. Updated by
    /// `SkillExecutor::record_execution`.
    pub success_rate: f32,
    /// Total executions recorded.
    pub execution_count: u32,
}

impl Skill {
    /// Construct a fresh skill with timestamps set to "now" and
    /// counters zeroed. `id` defaults to a UUID v4 if not provided.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        prompt_template: impl Into<String>,
        author: impl Into<String>,
    ) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            version: 1,
            description: description.into(),
            tier: SkillTier::Candidate,
            capability_tags: Vec::new(),
            params: Vec::new(),
            prompt_template: prompt_template.into(),
            required_tools: Vec::new(),
            success_examples: Vec::new(),
            author: author.into(),
            created_at: now,
            last_updated: now,
            success_rate: 0.0,
            execution_count: 0,
        }
    }

    /// Builder: add a capability tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.capability_tags.push(tag.into());
        self
    }

    /// Builder: add a required tool.
    pub fn with_required_tool(mut self, tool: impl Into<String>) -> Self {
        self.required_tools.push(tool.into());
        self
    }

    /// Builder: add a param spec.
    pub fn with_param(mut self, param: SkillParam) -> Self {
        self.params.push(param);
        self
    }

    /// Builder: add a success example (capped at 5).
    pub fn with_example(mut self, example: impl Into<String>) -> Self {
        if self.success_examples.len() < 5 {
            self.success_examples.push(example.into());
        }
        self
    }

    /// Builder: set the tier (default is Candidate).
    pub fn with_tier(mut self, tier: SkillTier) -> Self {
        self.tier = tier;
        self
    }
}

/// An immutable snapshot of a skill at a specific `version`.
///
/// `skill_versions` is append-only - when a skill is updated, a new
/// row is inserted with `version = old_version + 1` and the full YAML
/// payload is stored verbatim for replay / rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillVersion {
    /// Composite primary key: `(skill_id, version)`.
    pub skill_id: String,
    pub version: u32,
    /// Full YAML of the [`Skill`] at the time of this version. Stored
    /// verbatim so a rollback can re-hydrate the row without
    /// re-deriving fields.
    pub yaml: String,
    /// Unix epoch milliseconds.
    pub created_at: i64,
    /// Free-form changelog entry ("Added support for nested JSON"...).
    pub changelog: String,
}

/// Per-execution telemetry record. Inserted by
/// `SkillExecutor::record_execution` after every run, success or
/// failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillExecutionRecord {
    pub skill_id: String,
    pub success: bool,
    pub latency_ms: u32,
    /// Unix epoch milliseconds.
    pub timestamp: i64,
    /// JSON-encoded input parameters (for replay / debugging).
    pub params_json: String,
    /// Optional error message (only set on failure).
    pub error: Option<String>,
}

#[cfg(test)]
mod skill_type_tests {
    use super::*;

    #[test]
    fn skill_tier_as_str_is_snake_case() {
        assert_eq!(SkillTier::Candidate.as_str(), "candidate");
        assert_eq!(SkillTier::Active.as_str(),    "active");
        assert_eq!(SkillTier::Inactive.as_str(),  "inactive");
        assert_eq!(SkillTier::Archived.as_str(),  "archived");
    }

    #[test]
    fn skill_tier_serde_roundtrip() {
        for t in SkillTier::ALL {
            let s = serde_json::to_string(&t).unwrap();
            let back: SkillTier = serde_json::from_str(&s).unwrap();
            assert_eq!(t, back, "roundtrip failed for {t}");
        }
    }

    #[test]
    fn skill_tier_serde_is_snake_case() {
        assert_eq!(serde_json::to_string(&SkillTier::Candidate).unwrap(), "\"candidate\"");
        assert_eq!(serde_json::to_string(&SkillTier::Active).unwrap(),    "\"active\"");
        assert_eq!(serde_json::to_string(&SkillTier::Inactive).unwrap(),  "\"inactive\"");
        assert_eq!(serde_json::to_string(&SkillTier::Archived).unwrap(),  "\"archived\"");
    }

    #[test]
    fn only_active_tier_is_retrievable() {
        assert!(!SkillTier::Candidate.is_retrievable());
        assert!( SkillTier::Active.is_retrievable());
        assert!(!SkillTier::Inactive.is_retrievable());
        assert!(!SkillTier::Archived.is_retrievable());
    }

    #[test]
    fn skill_new_sets_defaults() {
        let s = Skill::new(
            "convert-csv-to-json",
            "Convert a CSV string into a JSON array of objects.",
            "Convert this CSV to JSON:\n```\n{{csv}}\n```",
            "user:alice",
        );
        assert_eq!(s.name, "convert-csv-to-json");
        assert_eq!(s.version, 1);
        assert_eq!(s.tier, SkillTier::Candidate);
        assert_eq!(s.author, "user:alice");
        assert_eq!(s.execution_count, 0);
        assert_eq!(s.success_rate, 0.0);
        assert!(!s.id.is_empty());
        assert!(s.created_at > 0);
        assert_eq!(s.created_at, s.last_updated);
    }

    #[test]
    fn skill_builder_chain_works() {
        let s = Skill::new("n", "d", "p", "a")
            .with_tag("csv")
            .with_tag("data")
            .with_required_tool("file_read")
            .with_param(SkillParam {
                name: "csv".into(),
                type_: "string".into(),
                description: "Input CSV".into(),
                required: true,
            })
            .with_example("Convert foo,bar\n1,2 to JSON")
            .with_tier(SkillTier::Active);

        assert_eq!(s.capability_tags, vec!["csv", "data"]);
        assert_eq!(s.required_tools, vec!["file_read"]);
        assert_eq!(s.params.len(), 1);
        assert_eq!(s.params[0].name, "csv");
        assert_eq!(s.success_examples.len(), 1);
        assert_eq!(s.tier, SkillTier::Active);
    }

    #[test]
    fn skill_caps_success_examples_at_five() {
        let s2 = Skill::new("n2", "d", "p", "a");
        let mut built = s2;
        for i in 0..10 {
            built = built.with_example(format!("e{i}"));
        }
        assert_eq!(built.success_examples.len(), 5);
    }

    #[test]
    fn skill_serde_roundtrip() {
        let s = Skill::new("n", "d", "p", "a")
            .with_tag("x")
            .with_tier(SkillTier::Active);
        let json = serde_json::to_string(&s).unwrap();
        let back: Skill = serde_json::from_str(&json).unwrap();
        assert_eq!(s.id, back.id);
        assert_eq!(s.name, back.name);
        assert_eq!(s.tier, back.tier);
        assert_eq!(s.capability_tags, back.capability_tags);
    }

    #[test]
    fn skill_version_serde_roundtrip() {
        let v = SkillVersion {
            skill_id: "sk-1".into(),
            version: 3,
            yaml: "name: foo\n".into(),
            created_at: 1_700_000_000_000,
            changelog: "Initial commit".into(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: SkillVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(v.skill_id, back.skill_id);
        assert_eq!(v.version, back.version);
        assert_eq!(v.yaml, back.yaml);
    }

    #[test]
    fn skill_execution_record_serde_roundtrip() {
        let r = SkillExecutionRecord {
            skill_id: "sk-1".into(),
            success: true,
            latency_ms: 240,
            timestamp: 1_700_000_000_000,
            params_json: r#"{"csv":"a,b\n1,2"}"#.into(),
            error: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: SkillExecutionRecord = serde_json::from_str(&json).unwrap();
        assert!(back.success);
        assert_eq!(back.latency_ms, 240);
        assert!(back.error.is_none());
    }

    #[test]
    fn skill_execution_record_carries_error() {
        let r = SkillExecutionRecord {
            skill_id: "sk-1".into(),
            success: false,
            latency_ms: 5000,
            timestamp: 1_700_000_000_000,
            params_json: "{}".into(),
            error: Some("timeout".into()),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: SkillExecutionRecord = serde_json::from_str(&json).unwrap();
        assert!(!back.success);
        assert_eq!(back.error.as_deref(), Some("timeout"));
    }
}
