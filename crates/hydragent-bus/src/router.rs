use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use crate::message::{JsonRpcRequest, JsonRpcResponse, JsonRpcError, ERR_METHOD_NOT_FOUND};

#[async_trait]
pub trait MethodHandler: Send + Sync {
    async fn handle(&self, request: JsonRpcRequest, response_tx: mpsc::Sender<String>) -> JsonRpcResponse;
}

pub struct Router {
    handlers: HashMap<String, Arc<dyn MethodHandler>>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn register(&mut self, method: &str, handler: impl MethodHandler + 'static) {
        self.handlers.insert(method.to_string(), Arc::new(handler));
    }

    pub async fn route(&self, request: JsonRpcRequest, response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
        let method = request.method.clone();
        let id = request.id.clone();
        if let Some(handler) = self.handlers.get(&method) {
            handler.handle(request, response_tx).await
        } else {
            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(JsonRpcError {
                    code: ERR_METHOD_NOT_FOUND,
                    message: format!("Method '{}' not found", method),
                    data: None,
                }),
                id,
            }
        }
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct EchoHandler;

    #[async_trait]
    impl MethodHandler for EchoHandler {
        async fn handle(&self, request: JsonRpcRequest, _response_tx: mpsc::Sender<String>) -> JsonRpcResponse {
            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(request.params),
                error: None,
                id: request.id,
            }
        }
    }

    #[tokio::test]
    async fn test_router_success() {
        let mut router = Router::new();
        router.register("echo", EchoHandler);

        let (tx, _rx) = mpsc::channel(10);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "echo".to_string(),
            params: json!({"hello": "world"}),
            id: "1".to_string(),
        };

        let response = router.route(request, tx).await;
        assert_eq!(response.result.unwrap()["hello"], "world");
        assert!(response.error.is_none());
    }
}
