use async_trait::async_trait;
use hydragent_types::{PermissionTier, ToolResult, ToolStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;

#[derive(Serialize, Deserialize)]
struct ScheduleTaskParams {
    cron_expr: String,
    description: String,
    task_type: String,
    task_params: String,
    target_channel_id: String,
}

pub struct ScheduleTaskTool {
    schedule_fn: Arc<
        dyn Fn(String, String, String, String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>> + Send + Sync
    >,
}

impl ScheduleTaskTool {
    pub fn new<F>(schedule_fn: F) -> Self
    where
        F: Fn(String, String, String, String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>> + Send + Sync + 'static,
    {
        Self {
            schedule_fn: Arc::new(schedule_fn),
        }
    }
}

#[async_trait]
impl crate::tool_trait::Tool for ScheduleTaskTool {
    fn name(&self) -> &str {
        "schedule_task"
    }

    fn description(&self) -> &str {
        "Schedule an autonomous task to run on a recurring cron schedule. Use when the user asks you to do something automatically on a recurring basis."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "cron_expr": {
                    "type": "string",
                    "description": "Standard cron expression (5 fields: min hour day month day-of-week). E.g. '0 9 * * *' (every day at 9am)"
                },
                "description": {
                    "type": "string",
                    "description": "Human-readable description of what this task does"
                },
                "task_type": {
                    "type": "string",
                    "description": "Type of task. Use 'react_loop' to run a full LLM ReAct loop, or 'heartbeat' to push a static message via the HeartbeatEngine. 'work_iq_digest' is reserved and auto-set by rss_subscribe."
                },
                "task_params": {
                    "type": "string",
                    "description": "JSON string of parameters for the task"
                },
                "target_channel_id": {
                    "type": "string",
                    "description": "Channel ID to send results to (e.g. 'telegram:123456789')"
                }
            },
            "required": ["cron_expr", "description", "task_type", "task_params", "target_channel_id"]
        }"#
    }

    fn permission_tier(&self) -> PermissionTier {
        PermissionTier::Prompt
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let params: ScheduleTaskParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id: "".to_string(),
                    output_json: "".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Invalid parameters: {}", e)),
                };
            }
        };

        match (self.schedule_fn)(
            params.cron_expr,
            params.description,
            params.task_type,
            params.task_params,
            params.target_channel_id,
        ).await {
            Ok(job_id) => ToolResult {
                call_id: "".to_string(),
                output_json: format!(r#"{{"status":"scheduled","job_id":"{}"}}"#, job_id),
                status: ToolStatus::Success,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: None,
            },
            Err(e) => ToolResult {
                call_id: "".to_string(),
                output_json: "".to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some(format!("Failed to schedule task: {}", e)),
            },
        }
    }
}
