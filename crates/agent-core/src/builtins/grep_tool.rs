use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

use crate::tools::BuiltinTool;
use crate::types::{ToolDefinition, ToolResult};

const MAX_RESULTS: usize = 250;

#[derive(Default)]
pub struct GrepTool;

#[async_trait]
impl BuiltinTool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".into(),
            description:
                "Search file contents with a regex pattern. Returns matching lines with file paths and line numbers."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "The regex pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search in (default: current directory)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "Glob pattern to filter files (e.g., \"*.rs\", \"*.ts\")"
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
        let pattern_str = match ToolResult::require_str(&input, "pattern") {
            Ok(p) => p.to_string(),
            Err(e) => return e,
        };

        let search_path = input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let glob_filter = input.get("glob").and_then(|v| v.as_str()).map(String::from);

        // Run blocking directory walk + file reads off the async runtime
        let result = tokio::task::spawn_blocking(move || {
            let regex = match regex::Regex::new(&pattern_str) {
                Ok(r) => r,
                Err(e) => return Err(format!("Invalid regex pattern: {e}")),
            };

            let glob_pattern = glob_filter.and_then(|g| glob::Pattern::new(&g).ok());
            let path = Path::new(&search_path);
            let mut results = Vec::new();

            if path.is_file() {
                search_file(&regex, path, &mut results);
            } else {
                for entry in walkdir::WalkDir::new(path)
                    .follow_links(false)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if !entry.file_type().is_file() {
                        continue;
                    }

                    if let Some(ref gp) = glob_pattern {
                        let file_name = entry.file_name().to_string_lossy();
                        if !gp.matches(&file_name) {
                            continue;
                        }
                    }

                    search_file(&regex, entry.path(), &mut results);

                    if results.len() >= MAX_RESULTS {
                        break;
                    }
                }
            }

            Ok(results)
        })
        .await;

        match result {
            Ok(Ok(results)) => {
                if results.is_empty() {
                    ToolResult::ok("No matches found.")
                } else {
                    let truncated = results.len() >= MAX_RESULTS;
                    let mut content = results.join("\n");
                    if truncated {
                        content.push_str(&format!("\n\n(truncated at {MAX_RESULTS} results)"));
                    }
                    ToolResult::ok(content)
                }
            }
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("Search task failed: {e}")),
        }
    }
}

fn search_file(regex: &regex::Regex, path: &Path, results: &mut Vec<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for (line_num, line) in content.lines().enumerate() {
        if regex.is_match(line) {
            results.push(format!("{}:{}:{}", path.display(), line_num + 1, line));
            if results.len() >= MAX_RESULTS {
                return;
            }
        }
    }
}
