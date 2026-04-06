use std::collections::HashSet;

use crate::tools::{PermissionCheck, PermissionPolicy};
use crate::types::ToolCall;

/// Evaluates a chain of permission policies in order. First `Some(...)` wins.
pub struct PolicyChain {
    policies: Vec<Box<dyn PermissionPolicy>>,
}

impl PolicyChain {
    pub fn new(policies: Vec<Box<dyn PermissionPolicy>>) -> Self {
        Self { policies }
    }

    /// Build the default four-layer policy chain.
    pub fn default_chain() -> Self {
        Self::new(vec![
            Box::new(DenyListPolicy::default()),
            Box::new(AllowListPolicy::default()),
            Box::new(DestructivePatternPolicy),
            Box::new(DefaultRequiresApprovalPolicy),
        ])
    }

    /// Evaluate all policies in order. Returns the first match,
    /// or `RequiresApproval` if no policy matched (should not happen
    /// with `DefaultRequiresApprovalPolicy` as the last layer).
    pub fn check(&self, tool_call: &ToolCall) -> PermissionCheck {
        for policy in &self.policies {
            if let Some(result) = policy.check(tool_call) {
                return result;
            }
        }
        PermissionCheck::RequiresApproval("No policy matched".into())
    }
}

/// Layer 1: Blocks explicitly dangerous commands/tools.
pub struct DenyListPolicy {
    pub patterns: Vec<String>,
}

impl Default for DenyListPolicy {
    fn default() -> Self {
        Self {
            patterns: vec![
                "rm -rf /".into(),
                "mkfs".into(),
                "dd if=".into(),
                ":(){:|:&};:".into(),
            ],
        }
    }
}

impl PermissionPolicy for DenyListPolicy {
    fn check(&self, tool_call: &ToolCall) -> Option<PermissionCheck> {
        // Only inspect the "command" field for bash-like tools, avoiding
        // expensive serialization of large inputs (e.g., file_write content).
        let command = tool_call
            .input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let lower = command.to_lowercase();
        for pattern in &self.patterns {
            if lower.contains(pattern) {
                return Some(PermissionCheck::Deny(format!(
                    "Command matches deny-listed pattern: {pattern}"
                )));
            }
        }
        None
    }
}

/// Layer 2: Permits known-safe tools without requiring approval.
pub struct AllowListPolicy {
    pub tool_names: HashSet<String>,
}

impl Default for AllowListPolicy {
    fn default() -> Self {
        Self {
            tool_names: ["file_read", "glob", "grep", "memory_fetch", "memory_list"]
                .into_iter()
                .map(String::from)
                .collect(),
        }
    }
}

impl PermissionPolicy for AllowListPolicy {
    fn check(&self, tool_call: &ToolCall) -> Option<PermissionCheck> {
        // Strip MCP namespace prefix if present (e.g., "skill__tool" -> "tool")
        let bare_name = tool_call.name.split("__").last().unwrap_or(&tool_call.name);
        if self.tool_names.contains(bare_name) || self.tool_names.contains(&tool_call.name) {
            return Some(PermissionCheck::Allow);
        }
        None
    }
}

/// Layer 3: Flags tools matching destructive patterns.
/// Migrated from `skill_registry::is_destructive()`.
pub struct DestructivePatternPolicy;

/// Patterns in tool names that indicate a destructive/side-effectful action.
pub const DESTRUCTIVE_PATTERNS: &[&str] = &["delete", "remove", "send", "drop"];

/// Explicit tool names known to be destructive.
pub const DESTRUCTIVE_EXPLICIT: &[&str] = &[
    "create_pull_request",
    "merge_pull_request",
    "issue_write",
    "manage_event",
    "create_or_update_file",
    "push_files",
];

impl PermissionPolicy for DestructivePatternPolicy {
    fn check(&self, tool_call: &ToolCall) -> Option<PermissionCheck> {
        let lower = tool_call.name.to_lowercase();
        let is_destructive = DESTRUCTIVE_PATTERNS
            .iter()
            .any(|pattern| lower.contains(pattern))
            || DESTRUCTIVE_EXPLICIT.iter().any(|name| lower.contains(name));

        if is_destructive {
            Some(PermissionCheck::RequiresApproval(format!(
                "Tool '{}' matches destructive pattern",
                tool_call.name
            )))
        } else {
            None
        }
    }
}

/// Layer 4: Catch-all — unknown tools require approval.
pub struct DefaultRequiresApprovalPolicy;

impl PermissionPolicy for DefaultRequiresApprovalPolicy {
    fn check(&self, tool_call: &ToolCall) -> Option<PermissionCheck> {
        Some(PermissionCheck::RequiresApproval(format!(
            "Tool '{}' has no explicit permission — requires approval",
            tool_call.name
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool_call(name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "test_id".into(),
            name: name.into(),
            input,
        }
    }

    #[test]
    fn test_deny_list_blocks_dangerous_commands() {
        let chain = PolicyChain::default_chain();
        let tc = make_tool_call("bash", json!({"command": "rm -rf /"}));
        match chain.check(&tc) {
            PermissionCheck::Deny(reason) => assert!(reason.contains("deny-listed")),
            other => panic!("Expected Deny, got {:?}", other),
        }
    }

    #[test]
    fn test_allow_list_permits_safe_tools() {
        let chain = PolicyChain::default_chain();
        let tc = make_tool_call("file_read", json!({"path": "/tmp/test"}));
        assert!(matches!(chain.check(&tc), PermissionCheck::Allow));
    }

    #[test]
    fn test_allow_list_permits_safe_tools_glob() {
        let chain = PolicyChain::default_chain();
        let tc = make_tool_call("glob", json!({"pattern": "*.rs"}));
        assert!(matches!(chain.check(&tc), PermissionCheck::Allow));
    }

    #[test]
    fn test_destructive_pattern_catches_delete() {
        let chain = PolicyChain::default_chain();
        let tc = make_tool_call("github__delete_branch", json!({}));
        match chain.check(&tc) {
            PermissionCheck::RequiresApproval(reason) => {
                assert!(reason.contains("destructive"))
            }
            other => panic!("Expected RequiresApproval, got {:?}", other),
        }
    }

    #[test]
    fn test_destructive_explicit_names() {
        let chain = PolicyChain::default_chain();
        let tc = make_tool_call("github__create_pull_request", json!({}));
        assert!(matches!(
            chain.check(&tc),
            PermissionCheck::RequiresApproval(_)
        ));
    }

    #[test]
    fn test_unknown_tool_requires_approval() {
        let chain = PolicyChain::default_chain();
        let tc = make_tool_call("some_unknown_tool", json!({}));
        match chain.check(&tc) {
            PermissionCheck::RequiresApproval(reason) => {
                assert!(reason.contains("no explicit permission"))
            }
            other => panic!("Expected RequiresApproval, got {:?}", other),
        }
    }

    #[test]
    fn test_deny_overrides_allow() {
        // Even if tool name is in allow list, deny-listed input should block
        let chain = PolicyChain::default_chain();
        let tc = make_tool_call("file_read", json!({"command": "rm -rf /"}));
        // DenyListPolicy checks input, so this should be denied
        assert!(matches!(chain.check(&tc), PermissionCheck::Deny(_)));
    }

    #[test]
    fn test_memory_fetch_allowed() {
        let chain = PolicyChain::default_chain();
        let tc = make_tool_call("memory_fetch", json!({"key": "test"}));
        assert!(matches!(chain.check(&tc), PermissionCheck::Allow));
    }
}
