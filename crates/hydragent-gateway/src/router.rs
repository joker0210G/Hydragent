use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use hydragent_types::{IntentEvent, AgentResponse, PushMessage};
use crate::{ChannelAdapterBridge, Deduplicator, RateLimiter};

pub struct GatewayRouter {
    adapters: RwLock<HashMap<String, Arc<dyn ChannelAdapterBridge>>>,
    dedup: Deduplicator,
    rate_limiters: RwLock<HashMap<String, RateLimiter>>,
}

impl GatewayRouter {
    pub fn new() -> Self {
        Self {
            adapters: RwLock::new(HashMap::new()),
            dedup: Deduplicator::new(1000),
            rate_limiters: RwLock::new(HashMap::new()),
        }
    }

    pub fn register_adapter(&self, channel_id: String, adapter: Arc<dyn ChannelAdapterBridge>) {
        tracing::info!(channel_id = %channel_id, "Channel adapter registered");
        self.adapters.write().insert(channel_id.clone(), adapter);
        self.rate_limiters.write().insert(
            channel_id.clone(),
            RateLimiter::default_for_channel(&channel_id),
        );
    }

    pub fn unregister_adapter(&self, channel_id: &str) {
        tracing::info!(channel_id = %channel_id, "Channel adapter unregistered");
        self.adapters.write().remove(channel_id);
        self.rate_limiters.write().remove(channel_id);
    }

    /// Check if inbound event is allowed (i.e. not duplicate & within rate limit)
    pub fn inbound_check(&self, request_id: &str, event: &IntentEvent) -> bool {
        if self.dedup.is_duplicate(
            &event.channel_id,
            &event.user_id,
            &event.content,
            request_id,
        ) {
            tracing::debug!(channel_id = %event.channel_id, "Dropped duplicate message");
            return false;
        }

        let mut limiters = self.rate_limiters.write();
        let limiter = limiters.entry(event.channel_id.clone()).or_insert_with(|| {
            RateLimiter::default_for_channel(&event.channel_id)
        });

        if !limiter.try_acquire() {
            tracing::warn!(channel_id = %event.channel_id, "Rate limit exceeded — message dropped");
            return false;
        }

        true
    }

    pub async fn outbound(&self, channel_id: &str, response: AgentResponse) -> anyhow::Result<()> {
        let adapter = self.adapters.read().get(channel_id).cloned();
        match adapter {
            Some(a) => a.send_response(response).await,
            None => {
                tracing::debug!(channel_id, "No adapter registered for channel (might be transient CLI connection)");
                Ok(())
            }
        }
    }

    pub async fn push(&self, msg: PushMessage) -> anyhow::Result<()> {
        if msg.channel_id == "*" {
            let adapters: Vec<Arc<dyn ChannelAdapterBridge>> = self.adapters.read().values().cloned().collect();
            for adapter in adapters {
                let _ = adapter.send_push(msg.clone()).await;
            }
            return Ok(());
        }

        let adapter = self.adapters.read().get(&msg.channel_id).cloned();
        match adapter {
            Some(a) => a.send_push(msg).await,
            None => {
                anyhow::bail!("No adapter registered for channel: {}", msg.channel_id);
            }
        }
    }
}

impl Default for GatewayRouter {
    fn default() -> Self {
        Self::new()
    }
}
