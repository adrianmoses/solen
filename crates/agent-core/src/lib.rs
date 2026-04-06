pub mod agent;
pub mod error;
pub mod llm;
pub mod permissions;
pub mod tools;
pub mod types;

#[cfg(feature = "native")]
pub mod builtins;

pub use agent::Agent;
pub use error::AgentError;
#[cfg(feature = "native")]
pub use llm::ReqwestBackend;
pub use llm::{HttpBackend, HttpResponse, LlmClient, LlmConfig};
pub use permissions::PolicyChain;
pub use tools::{BuiltinTool, PermissionCheck, PermissionPolicy};
pub use types::{
    AgentContext, AgentRunResult, ContentBlock, Message, Role, ToolCall, ToolDefinition,
    ToolExecutor, ToolResult,
};
