use async_trait::async_trait;
use hydragent_types::{AgentResponse, PushMessage};

#[async_trait]
pub trait ChannelAdapterBridge: Send + Sync {
    async fn send_response(&self, response: AgentResponse) -> anyhow::Result<()>;
    async fn send_push(&self, push: PushMessage) -> anyhow::Result<()>;
}
