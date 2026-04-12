use std::collections::HashMap;
use std::sync::Arc;

use agent_core::builtins::{
    BashTool, FileEditTool, FileReadTool, FileWriteTool, GlobTool, GrepTool,
};
use agent_core::{
    AgentError, BuiltinTool, PermissionCheck, PolicyChain, ToolCall, ToolDefinition, ToolExecutor,
    ToolResult,
};
use async_trait::async_trait;
use skill_registry::SkillRegistry;
use sqlx::SqlitePool;

use crate::builtins::{MemoryDeleteTool, MemoryFetchTool, MemoryListTool, MemoryStoreTool};

/// Composite executor that checks built-in tools first, then falls through
/// to the MCP `SkillRegistry` for external skill tools. Uses a `PolicyChain`
/// for permission decisions instead of the old `is_destructive()` check.
pub struct BuiltinExecutor {
    builtins: HashMap<String, Arc<dyn BuiltinTool>>,
    registry: Arc<SkillRegistry<agent_core::ReqwestBackend>>,
    policy: PolicyChain,
}

impl BuiltinExecutor {
    pub fn new(
        pool: SqlitePool,
        user_id: String,
        registry: SkillRegistry<agent_core::ReqwestBackend>,
        policy: PolicyChain,
    ) -> Self {
        let mut builtins: HashMap<String, Arc<dyn BuiltinTool>> = HashMap::new();

        builtins.insert("bash".into(), Arc::new(BashTool::new()));
        builtins.insert("file_read".into(), Arc::<FileReadTool>::default());
        builtins.insert("file_write".into(), Arc::<FileWriteTool>::default());
        builtins.insert("file_edit".into(), Arc::<FileEditTool>::default());
        builtins.insert("glob".into(), Arc::<GlobTool>::default());
        builtins.insert("grep".into(), Arc::<GrepTool>::default());

        builtins.insert(
            "memory_store".into(),
            Arc::new(MemoryStoreTool::new(pool.clone(), user_id.clone())),
        );
        builtins.insert(
            "memory_fetch".into(),
            Arc::new(MemoryFetchTool::new(pool.clone(), user_id.clone())),
        );
        builtins.insert(
            "memory_list".into(),
            Arc::new(MemoryListTool::new(pool.clone(), user_id.clone())),
        );
        builtins.insert(
            "memory_delete".into(),
            Arc::new(MemoryDeleteTool::new(pool, user_id)),
        );

        Self {
            builtins,
            registry: Arc::new(registry),
            policy,
        }
    }

    /// Check whether a tool call is allowed, denied, or requires approval.
    pub fn check_permission(&self, tool_call: &ToolCall) -> PermissionCheck {
        self.policy.check(tool_call)
    }

    /// Returns all tool definitions: built-in tools + MCP skill tools.
    pub fn all_tools(&self) -> Vec<ToolDefinition> {
        let mut tools: Vec<ToolDefinition> =
            self.builtins.values().map(|t| t.definition()).collect();
        tools.extend(self.registry.all_tools());
        tools
    }
}

#[async_trait]
impl ToolExecutor for BuiltinExecutor {
    async fn execute(&self, tool_call: &ToolCall) -> Result<ToolResult, AgentError> {
        // Try built-in tools first
        if let Some(builtin) = self.builtins.get(&tool_call.name) {
            let mut result = builtin.execute(tool_call.input.clone()).await;
            result.tool_use_id = tool_call.id.clone();
            return Ok(result);
        }

        // Fall through to MCP skill registry
        self.registry.execute(tool_call).await
    }

    fn is_concurrent_safe(&self, tool_call: &ToolCall) -> bool {
        if let Some(builtin) = self.builtins.get(&tool_call.name) {
            return builtin.is_concurrent_safe();
        }
        false
    }
}
