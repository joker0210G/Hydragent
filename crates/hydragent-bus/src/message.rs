// crates/hydragent-bus/src/message.rs
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,     // always "2.0"
    pub method: String,
    pub params: Value,
    pub id: String,          // UUID v4
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

// Standard error codes
pub const ERR_PARSE:            i32 = -32700;
pub const ERR_INVALID_REQUEST:  i32 = -32600;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERR_INTERNAL:         i32 = -32603;
// Hydragent-specific codes
pub const ERR_TOOL_FAILED:      i32 = -32001;
pub const ERR_LLM_UNAVAILABLE:  i32 = -32002;
pub const ERR_CONSENT_DENIED:   i32 = -32003;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_request_serialization() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "test.method".to_string(),
            params: json!({"arg": 1}),
            id: "uuid-123".to_string(),
        };

        let serialized = serde_json::to_string(&request).unwrap();
        let deserialized: JsonRpcRequest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.method, "test.method");
        assert_eq!(deserialized.id, "uuid-123");
    }

    #[test]
    fn test_response_serialization() {
        let response = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(json!({"status": "ok"})),
            error: None,
            id: "uuid-123".to_string(),
        };

        let serialized = serde_json::to_string(&response).unwrap();
        let deserialized: JsonRpcResponse = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.id, "uuid-123");
        assert_eq!(deserialized.result.unwrap()["status"], "ok");
    }
}

