use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

use crate::tools::BuiltinTool;
use crate::types::{ToolDefinition, ToolResult};

pub struct BashTool {
    default_timeout: Duration,
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
        }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BuiltinTool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".into(),
            description: "Execute a shell command and return its output (stdout + stderr).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "timeout_ms": {
                        "type": "number",
                        "description": "Optional timeout in milliseconds (default: 120000)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrent_safe(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let command = match ToolResult::require_str(&input, "command") {
            Ok(c) => c.to_string(),
            Err(e) => return e,
        };

        let timeout = input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(Duration::from_millis)
            .unwrap_or(self.default_timeout);

        let result = tokio::time::timeout(
            timeout,
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut content = String::new();
                if !stdout.is_empty() {
                    content.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str("STDERR:\n");
                    content.push_str(&stderr);
                }
                if content.is_empty() {
                    content = "(no output)".into();
                }
                ToolResult {
                    tool_use_id: String::new(),
                    content,
                    is_error: !output.status.success(),
                }
            }
            Ok(Err(e)) => ToolResult::err(format!("Failed to execute command: {e}")),
            Err(_) => ToolResult::err(format!("Command timed out after {}ms", timeout.as_millis())),
        }
    }
}
