use std::sync::Arc;
use hydragent_types::PushMessage;
use hydragent_gateway::GatewayRouter;

pub struct HeartbeatEngine {
    router: Arc<GatewayRouter>,
}

impl HeartbeatEngine {
    pub fn new(router: Arc<GatewayRouter>) -> Self {
        Self { router }
    }

    pub async fn push(
        &self,
        channel_id: String,
        page_id: String,
        content: String,
    ) -> anyhow::Result<()> {
        tracing::info!(
            channel_id = %channel_id,
            page_id = %page_id,
            content_len = content.len(),
            "Heartbeat pushing proactive message"
        );

        self.router.push(PushMessage {
            channel_id,
            page_id,
            content,
            markdown: true,
            metadata: std::collections::HashMap::new(),
        }).await
    }
}
