use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tools::BuiltinTool;
use crate::types::{ToolDefinition, ToolResult};

#[derive(Default)]
pub struct FileEditTool;

#[async_trait]
impl BuiltinTool for FileEditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file_edit".into(),
            description:
                "Apply a search-and-replace edit to a file. The old_text must match exactly once."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to edit"
                    },
                    "old_text": {
                        "type": "string",
                        "description": "The exact text to find and replace (must be unique in the file)"
                    },
                    "new_text": {
                        "type": "string",
                        "description": "The replacement text"
                    }
                },
                "required": ["path", "old_text", "new_text"]
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
        let old_text = match ToolResult::require_str(&input, "old_text") {
            Ok(t) => t.to_string(),
            Err(e) => return e,
        };
        let new_text = match ToolResult::require_str(&input, "new_text") {
            Ok(t) => t.to_string(),
            Err(e) => return e,
        };

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("Failed to read file '{path}': {e}")),
        };

        let count = content.matches(&*old_text).count();
        if count == 0 {
            return ToolResult::err(format!("old_text not found in '{path}'"));
        }
        if count > 1 {
            return ToolResult::err(format!(
                "old_text matches {count} times in '{path}' — must be unique. \
                 Provide more surrounding context to make it unique."
            ));
        }

        let updated = content.replacen(&*old_text, &new_text, 1);
        match tokio::fs::write(&path, &updated).await {
            Ok(()) => ToolResult::ok(format!("Successfully edited {path}")),
            Err(e) => ToolResult::err(format!("Failed to write file '{path}': {e}")),
        }
    }
}
