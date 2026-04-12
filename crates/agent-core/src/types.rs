use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::AgentError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    CompactBoundary {
        summary: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    /// Create an error result for a failed tool execution.
    pub fn error_for(tool_use_id: String, err: impl std::fmt::Display) -> Self {
        Self {
            tool_use_id,
            content: format!("Tool execution error: {err}"),
            is_error: true,
        }
    }

    /// Create a success result (for built-in tools; caller sets `tool_use_id`).
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            tool_use_id: String::new(),
            content: content.into(),
            is_error: false,
        }
    }

    /// Create an error result (for built-in tools; caller sets `tool_use_id`).
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            tool_use_id: String::new(),
            content: content.into(),
            is_error: true,
        }
    }

    /// Extract a required string field from JSON input, or return an error result.
    pub fn require_str<'a>(input: &'a serde_json::Value, field: &str) -> Result<&'a str, Self> {
        input
            .get(field)
            .and_then(|v| v.as_str())
            .ok_or_else(|| Self::err(format!("Missing required field: {field}")))
    }
}

impl From<ToolResult> for ContentBlock {
    fn from(r: ToolResult) -> Self {
        ContentBlock::ToolResult {
            tool_use_id: r.tool_use_id,
            content: r.content,
            is_error: r.is_error,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
}

#[cfg(feature = "native")]
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, tool_call: &ToolCall) -> Result<ToolResult, AgentError>;

    /// Return true if this tool can safely run concurrently with other tools.
    /// Default: false (sequential execution).
    fn is_concurrent_safe(&self, _tool_call: &ToolCall) -> bool {
        false
    }
}

#[cfg(not(feature = "native"))]
#[async_trait(?Send)]
pub trait ToolExecutor {
    async fn execute(&self, tool_call: &ToolCall) -> Result<ToolResult, AgentError>;

    /// Return true if this tool can safely run concurrently with other tools.
    fn is_concurrent_safe(&self, _tool_call: &ToolCall) -> bool {
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunResult {
    pub new_messages: Vec<Message>,
    pub answer: Option<String>,
    pub pending_tool_calls: Vec<ToolCall>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_boundary_serde_roundtrip() {
        let block = ContentBlock::CompactBoundary {
            summary: "Conversation summary here.".to_string(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"compact_boundary\""));
        assert!(json.contains("\"summary\":\"Conversation summary here.\""));

        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        match deserialized {
            ContentBlock::CompactBoundary { summary } => {
                assert_eq!(summary, "Conversation summary here.");
            }
            _ => panic!("Expected CompactBoundary variant"),
        }
    }

    #[test]
    fn test_tool_executor_default_methods() {
        // Verify a minimal ToolExecutor impl compiles with only execute()
        struct MinimalExecutor;

        #[cfg_attr(feature = "native", async_trait)]
        #[cfg_attr(not(feature = "native"), async_trait(?Send))]
        impl ToolExecutor for MinimalExecutor {
            async fn execute(&self, tool_call: &ToolCall) -> Result<ToolResult, AgentError> {
                Ok(ToolResult {
                    tool_use_id: tool_call.id.clone(),
                    content: "ok".to_string(),
                    is_error: false,
                })
            }
            // is_concurrent_safe uses default
        }

        let executor = MinimalExecutor;
        let tc = ToolCall {
            id: "1".to_string(),
            name: "test".to_string(),
            input: serde_json::json!({}),
        };
        assert!(!executor.is_concurrent_safe(&tc));
    }
}
