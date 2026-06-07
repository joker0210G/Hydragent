// crates/hydragent-types/src/lib.rs
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Inbound user message, normalised from any channel adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentEvent {
    /// UUID v4 — uniquely identifies the session
    pub session_id: String,
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
    pub session_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub session_id: String,
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
            session_id: "session-123".to_string(),
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
        assert_eq!(deserialized.session_id, "session-123");
        assert_eq!(deserialized.content, "hello");
    }

    #[test]
    fn test_agent_response_serialization() {
        let response = AgentResponse {
            session_id: "session-123".to_string(),
            content: "response content".to_string(),
            format: ResponseFormat::Markdown,
            consent_requests: vec![],
            tool_calls_executed: vec![],
        };

        let serialized = serde_json::to_string(&response).unwrap();
        let deserialized: AgentResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.session_id, "session-123");
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

