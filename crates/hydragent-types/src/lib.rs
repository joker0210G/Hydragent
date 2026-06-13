// crates/hydragent-types/src/lib.rs
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
