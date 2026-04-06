use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tools::BuiltinTool;
use crate::types::{ToolDefinition, ToolResult};

#[derive(Default)]
pub struct FileReadTool;

#[async_trait]
impl BuiltinTool for FileReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file_read".into(),
            description: "Read the contents of a file. Supports optional line offset and limit."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to read"
                    },
                    "offset": {
                        "type": "number",
                        "description": "Line number to start reading from (0-based, default: 0)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of lines to read (default: all)"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let path = match ToolResult::require_str(&input, "path") {
            Ok(p) => p.to_string(),
            Err(e) => return e,
        };

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("Failed to read file '{path}': {e}")),
        };

        let offset = input.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let lines: Vec<&str> = content.lines().collect();
        let selected: Vec<&str> = match limit {
            Some(lim) => lines.into_iter().skip(offset).take(lim).collect(),
            None => lines.into_iter().skip(offset).collect(),
        };

        let result = selected
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}\t{line}", offset + i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        ToolResult::ok(result)
    }
}
