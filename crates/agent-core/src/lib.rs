pub mod agent;
pub mod error;
pub mod llm;
pub mod types;

pub use agent::Agent;
pub use error::AgentError;
#[cfg(feature = "native")]
pub use llm::ReqwestBackend;
pub use llm::{HttpBackend, LlmClient, LlmConfig};
pub use types::{
    AgentContext, AgentRunResult, ContentBlock, Message, Role, ToolCall, ToolDefinition,
    ToolExecutor, ToolResult,
};
