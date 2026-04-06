use async_trait::async_trait;
use serde_json::Value;

use crate::types::{ToolCall, ToolDefinition, ToolResult};

/// Result of evaluating a permission policy against a tool call.
#[derive(Debug, Clone)]
pub enum PermissionCheck {
    /// Tool is explicitly allowed to execute without approval.
    Allow,
    /// Tool is explicitly denied. Contains the reason.
    Deny(String),
    /// Tool requires user approval before execution. Contains the reason.
    RequiresApproval(String),
}

/// A single layer in the permission policy chain.
///
/// Returns `Some(PermissionCheck)` if this policy has an opinion on the tool call,
/// or `None` to defer to the next policy in the chain.
#[cfg(feature = "native")]
pub trait PermissionPolicy: Send + Sync {
    fn check(&self, tool_call: &ToolCall) -> Option<PermissionCheck>;
}

#[cfg(not(feature = "native"))]
pub trait PermissionPolicy {
    fn check(&self, tool_call: &ToolCall) -> Option<PermissionCheck>;
}

/// A built-in tool that executes in-process (no MCP round-trip).
///
/// Built-in tools provide their own `ToolDefinition` for the LLM and handle
/// execution directly. The `tool_use_id` is set by the caller (BuiltinExecutor)
/// after `execute()` returns.
#[cfg(feature = "native")]
#[async_trait]
pub trait BuiltinTool: Send + Sync {
    /// Returns the tool definition (name, description, JSON Schema) for the LLM.
    fn definition(&self) -> ToolDefinition;

    /// Whether this tool requires user approval for the given input.
    fn needs_approval(&self, input: &Value) -> bool;

    /// Whether this tool can safely run concurrently with other tools.
    fn is_concurrent_safe(&self) -> bool;

    /// Execute the tool with the given input. Returns a ToolResult with
    /// an empty `tool_use_id` — the caller must set it.
    async fn execute(&self, input: Value) -> ToolResult;
}

#[cfg(not(feature = "native"))]
#[async_trait(?Send)]
pub trait BuiltinTool {
    fn definition(&self) -> ToolDefinition;
    fn needs_approval(&self, input: &Value) -> bool;
    fn is_concurrent_safe(&self) -> bool;
    async fn execute(&self, input: Value) -> ToolResult;
}
