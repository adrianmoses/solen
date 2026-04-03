use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("LLM request failed: {0}")]
    LlmRequestFailed(String),

    #[error("LLM response parse error: {0}")]
    LlmResponseParse(String),

    #[error("Max iterations ({0}) exceeded")]
    MaxIterationsExceeded(u32),

    #[error("Unexpected stop reason: {0}")]
    UnexpectedStopReason(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("Tool execution failed: {0}")]
    ToolExecutionFailed(String),

    #[error("MCP error: {0}")]
    McpError(String),

    #[error("Skill not found: {0}")]
    SkillNotFound(String),

    #[error("Prompt too long, compaction required")]
    PromptTooLong,

    #[error("Max continuation attempts ({0}) exceeded")]
    MaxContinuationsExceeded(u32),
}
