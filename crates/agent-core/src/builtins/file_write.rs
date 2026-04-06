use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

use crate::tools::BuiltinTool;
use crate::types::{ToolDefinition, ToolResult};

#[derive(Default)]
pub struct FileWriteTool;

#[async_trait]
impl BuiltinTool for FileWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file_write".into(),
            description: "Write content to a file. Creates the file and parent directories if they don't exist. Overwrites existing content.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to write to"
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn is_concurrent_safe(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let path = match ToolResult::require_str(&input, "path") {
            Ok(p) => p.to_string(),
            Err(e) => return e,
        };
        let content = match ToolResult::require_str(&input, "content") {
            Ok(c) => c.to_string(),
            Err(e) => return e,
        };

        if let Some(parent) = Path::new(&path).parent() {
            if !parent.exists() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return ToolResult::err(format!(
                        "Failed to create directories for '{path}': {e}"
                    ));
                }
            }
        }

        match tokio::fs::write(&path, &content).await {
            Ok(()) => ToolResult::ok(format!(
                "Successfully wrote {} bytes to {path}",
                content.len()
            )),
            Err(e) => ToolResult::err(format!("Failed to write file '{path}': {e}")),
        }
    }
}
