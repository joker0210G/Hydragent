use async_trait::async_trait;
use hydragent_types::{ToolResult, ToolStatus};
use crate::tool_trait::Tool;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::warn;

pub struct FileReadTool {
    workspace_dir: PathBuf,
}

impl FileReadTool {
    pub fn new(workspace_dir: impl Into<PathBuf>) -> Self {
        Self {
            workspace_dir: workspace_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a text file within the agent's workspace directory."
    }

    fn permission_tier(&self) -> hydragent_types::PermissionTier {
        hydragent_types::PermissionTier::Prompt
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path within the workspace directory (e.g. 'notes/todo.md')"
                }
            },
            "required": ["path"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start = std::time::Instant::now();
        let val: Value = match serde_json::from_str(params_json) {
            Ok(v) => v,
            Err(e) => {
                return ToolResult {
                    call_id: "".to_string(),
                    output_json: "{}".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Invalid parameters: {}", e)),
                };
            }
        };

        let relative_path_str = val.get("path").and_then(|p| p.as_str()).unwrap_or("");
        if relative_path_str.is_empty() {
            return ToolResult {
                call_id: "".to_string(),
                output_json: "{}".to_string(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some("Empty path parameter".to_string()),
            };
        }

        let relative_path = Path::new(relative_path_str);
        
        // Block raw relative back references in path parameter
        if relative_path_str.contains("..")
            || relative_path_str.starts_with('/')
            || relative_path_str.starts_with('\\')
            || relative_path.is_absolute()
        {
            return ToolResult {
                call_id: "".to_string(),
                output_json: "{}".to_string(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some("Security Warning: Directory traversal or absolute paths blocked".to_string()),
            };
        }


        let target_path = self.workspace_dir.join(relative_path);

        // Ensure directories exist
        if let Err(e) = fs::create_dir_all(&self.workspace_dir).await {
            return ToolResult {
                call_id: "".to_string(),
                output_json: "{}".to_string(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some(format!("Failed to create workspace directory: {}", e)),
            };
        }

        // Canonicalize base path
        let canonical_base = match self.workspace_dir.canonicalize() {
            Ok(path) => path,
            Err(e) => {
                return ToolResult {
                    call_id: "".to_string(),
                    output_json: "{}".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: 0,
                    error_message: Some(format!("Invalid base path configuration: {}", e)),
                };
            }
        };

        // Check file metadata (exists and size limits)
        let metadata = match fs::metadata(&target_path).await {
            Ok(m) => m,
            Err(e) => {
                return ToolResult {
                    call_id: "".to_string(),
                    output_json: "{}".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("File not found or inaccessible: {}", e)),
                };
            }
        };

        if !metadata.is_file() {
            return ToolResult {
                call_id: "".to_string(),
                output_json: "{}".to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some("Path points to a directory, not a file".to_string()),
            };
        }

        if metadata.len() > 512 * 1024 {
            return ToolResult {
                call_id: "".to_string(),
                output_json: "{}".to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some("File size exceeds 512 KB limit".to_string()),
            };
        }

        // Canonicalize target path to resolve references and check boundaries
        let canonical_target = match target_path.canonicalize() {
            Ok(path) => path,
            Err(e) => {
                return ToolResult {
                    call_id: "".to_string(),
                    output_json: "{}".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("Path canonicalization failed: {}", e)),
                };
            }
        };

        if !canonical_target.starts_with(&canonical_base) {
            warn!("Security Warning: Directory traversal blocked for path {:?}", canonical_target);
            return ToolResult {
                call_id: "".to_string(),
                output_json: "{}".to_string(),
                status: ToolStatus::Failure,
                execution_ms: start.elapsed().as_millis() as u32,
                error_message: Some("Security Warning: Directory traversal attempt blocked".to_string()),
            };
        }

        match fs::read_to_string(&canonical_target).await {
            Ok(content) => {
                let output = serde_json::json!({
                    "content": content,
                    "path": relative_path_str
                });
                ToolResult {
                    call_id: "".to_string(),
                    output_json: serde_json::to_string(&output).unwrap_or_default(),
                    status: ToolStatus::Success,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: None,
                }
            }
            Err(e) => {
                ToolResult {
                    call_id: "".to_string(),
                    output_json: "{}".to_string(),
                    status: ToolStatus::Failure,
                    execution_ms: start.elapsed().as_millis() as u32,
                    error_message: Some(format!("Failed to read file: {}", e)),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_file_read_safety_traversal() {
        let temp_dir = std::env::temp_dir();
        let tool = FileReadTool::new(&temp_dir);

        // Traversal attempt
        let res = tool.execute(r#"{"path": "../etc/passwd"}"#).await;
        assert_eq!(res.status, ToolStatus::Failure);
        assert!(res.error_message.unwrap().contains("traversal"));
    }

    #[tokio::test]
    async fn test_file_read_absolute_path() {
        let temp_dir = std::env::temp_dir();
        let tool = FileReadTool::new(&temp_dir);

        // Absolute path attempt
        let res = tool.execute(r#"{"path": "/etc/passwd"}"#).await;
        assert_eq!(res.status, ToolStatus::Failure);
        assert!(res.error_message.unwrap().contains("traversal"));
    }
}

