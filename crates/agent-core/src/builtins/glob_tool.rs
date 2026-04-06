use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tools::BuiltinTool;
use crate::types::{ToolDefinition, ToolResult};

const MAX_RESULTS: usize = 1000;

#[derive(Default)]
pub struct GlobTool;

#[async_trait]
impl BuiltinTool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "glob".into(),
            description: "Find files matching a glob pattern (e.g., \"**/*.rs\", \"src/**/*.ts\")."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "The glob pattern to match files against"
                    },
                    "path": {
                        "type": "string",
                        "description": "Base directory to search in (default: current directory)"
                    }
                },
                "required": ["pattern"]
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
        let pattern = match ToolResult::require_str(&input, "pattern") {
            Ok(p) => p.to_string(),
            Err(e) => return e,
        };

        let base = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let full_pattern = if pattern.starts_with('/') {
            pattern
        } else {
            format!("{base}/{pattern}")
        };

        match glob::glob(&full_pattern) {
            Ok(paths) => {
                let matches: Vec<String> = paths
                    .filter_map(|entry| entry.ok())
                    .take(MAX_RESULTS)
                    .map(|p| p.display().to_string())
                    .collect();

                if matches.is_empty() {
                    ToolResult::ok("No files matched the pattern.")
                } else {
                    let truncated = matches.len() >= MAX_RESULTS;
                    let mut content = matches.join("\n");
                    if truncated {
                        content.push_str(&format!("\n\n(truncated at {MAX_RESULTS} results)"));
                    }
                    ToolResult::ok(content)
                }
            }
            Err(e) => ToolResult::err(format!("Invalid glob pattern: {e}")),
        }
    }
}
