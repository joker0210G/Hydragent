use async_trait::async_trait;
use tokio::sync::mpsc;
use crate::openrouter::LLMRequest;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn provider_name(&self) -> &str;
    fn is_available(&self) -> bool;
    async fn chat_stream(
        &self,
        request: &LLMRequest,
        token_tx: mpsc::Sender<String>,
    ) -> anyhow::Result<String>;
}
