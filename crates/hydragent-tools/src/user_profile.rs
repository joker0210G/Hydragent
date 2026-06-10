use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use hydragent_types::{ToolResult, ToolStatus};
use crate::tool_trait::Tool;

pub struct UserProfileTool {
    config_dir: PathBuf,
}

impl UserProfileTool {
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }
}

#[derive(Deserialize)]
struct UserProfileParams {
    action: String, // "add" | "remove" | "list"
    habit: Option<String>,
}

#[async_trait]
impl Tool for UserProfileTool {
    fn name(&self) -> &str {
        "user_profile"
    }

    fn description(&self) -> &str {
        "Allows viewing, adding, or removing user profile habits or preferences in `./config/USER.md`."
    }

    fn params_schema(&self) -> &str {
        r#"{
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "remove", "list"],
                    "description": "The action to perform: 'add' a style habit/preference, 'remove' an existing one, or 'list' all profile elements."
                },
                "habit": {
                    "type": "string",
                    "description": "The habit/preference text to add or remove (e.g. 'The user likes concise answers')."
                }
            },
            "required": ["action"]
        }"#
    }

    async fn execute(&self, params_json: &str) -> ToolResult {
        let start_time = std::time::Instant::now();
        let call_id = uuid::Uuid::new_v4().to_string();

        let params: UserProfileParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult {
                    call_id,
                    output_json: "{}".into(),
                    status: ToolStatus::Failure,
                    execution_ms: start_time.elapsed().as_millis() as u32,
                    error_message: Some(format!("Invalid parameters: {}", e)),
                };
            }
        };

        let file_path = self.config_dir.join("USER.md");

        // Ensure config directory exists
        if let Some(parent) = file_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        match params.action.as_str() {
            "list" => {
                let content = fs::read_to_string(&file_path).unwrap_or_default();
                ToolResult {
                    call_id,
                    output_json: json!({
                        "status": "success",
                        "content": content
                    }).to_string(),
                    status: ToolStatus::Success,
                    execution_ms: start_time.elapsed().as_millis() as u32,
                    error_message: None,
                }
            }
            "add" => {
                let habit = match params.habit {
                    Some(h) => h,
                    None => {
                        return ToolResult {
                            call_id,
                            output_json: "{}".into(),
                            status: ToolStatus::Failure,
                            execution_ms: start_time.elapsed().as_millis() as u32,
                            error_message: Some("The 'habit' parameter is required for 'add' action.".into()),
                        };
                    }
                };

                let mut current_content = fs::read_to_string(&file_path).unwrap_or_else(|_| {
                    "# User Profile\n- Name: User\n\n# Style & Communication Habits\n".to_string()
                });
                if !current_content.contains("# Style & Communication Habits") {
                    current_content.push_str("\n# Style & Communication Habits\n");
                }
                if !current_content.ends_with('\n') && !current_content.is_empty() {
                    current_content.push('\n');
                }
                current_content.push_str(&format!("* {}\n", habit));

                if let Err(e) = fs::write(&file_path, current_content) {
                    return ToolResult {
                        call_id,
                        output_json: "{}".into(),
                        status: ToolStatus::Failure,
                        execution_ms: start_time.elapsed().as_millis() as u32,
                        error_message: Some(format!("Failed to write to USER.md: {}", e)),
                    };
                }

                ToolResult {
                    call_id,
                    output_json: json!({
                        "status": "success",
                        "message": format!("Habit added successfully: {}", habit)
                    }).to_string(),
                    status: ToolStatus::Success,
                    execution_ms: start_time.elapsed().as_millis() as u32,
                    error_message: None,
                }
            }
            "remove" => {
                let habit = match params.habit {
                    Some(h) => h,
                    None => {
                        return ToolResult {
                            call_id,
                            output_json: "{}".into(),
                            status: ToolStatus::Failure,
                            execution_ms: start_time.elapsed().as_millis() as u32,
                            error_message: Some("The 'habit' parameter is required for 'remove' action.".into()),
                        };
                    }
                };

                let content = fs::read_to_string(&file_path).unwrap_or_default();
                let lines: Vec<&str> = content.lines().collect();
                let mut new_lines = Vec::new();
                let mut found = false;

                for line in lines {
                    let normalized_line = line.trim_start_matches('*').trim_start_matches('-').trim();
                    if normalized_line == habit.trim() && !found {
                        found = true;
                        continue;
                    }
                    new_lines.push(line);
                }

                let new_content = new_lines.join("\n") + "\n";
                if let Err(e) = fs::write(&file_path, new_content) {
                    return ToolResult {
                        call_id,
                        output_json: "{}".into(),
                        status: ToolStatus::Failure,
                        execution_ms: start_time.elapsed().as_millis() as u32,
                        error_message: Some(format!("Failed to write to USER.md: {}", e)),
                    };
                }

                ToolResult {
                    call_id,
                    output_json: json!({
                        "status": "success",
                        "message": if found { format!("Habit removed successfully: {}", habit) } else { "Habit not found.".to_string() }
                    }).to_string(),
                    status: ToolStatus::Success,
                    execution_ms: start_time.elapsed().as_millis() as u32,
                    error_message: None,
                }
            }
            _ => ToolResult {
                call_id,
                output_json: "{}".into(),
                status: ToolStatus::Failure,
                execution_ms: start_time.elapsed().as_millis() as u32,
                error_message: Some("Invalid action parameter.".into()),
            },
        }
    }
}
