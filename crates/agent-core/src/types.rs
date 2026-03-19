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
}

#[cfg(not(feature = "native"))]
#[async_trait(?Send)]
pub trait ToolExecutor {
    async fn execute(&self, tool_call: &ToolCall) -> Result<ToolResult, AgentError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunResult {
    pub new_messages: Vec<Message>,
    pub answer: Option<String>,
    pub pending_tool_calls: Vec<ToolCall>,
}
