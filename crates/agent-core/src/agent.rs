use crate::error::AgentError;
use crate::llm::{HttpBackend, LlmClient, StopReason};
use crate::types::{
    AgentContext, AgentRunResult, ContentBlock, Message, Role, ToolCall, ToolResult,
};

pub struct Agent<H: HttpBackend> {
    pub llm: LlmClient<H>,
    pub max_iterations: u32,
}

impl<H: HttpBackend> Agent<H> {
    pub fn new(llm: LlmClient<H>) -> Self {
        Self {
            llm,
            max_iterations: 10,
        }
    }

    #[allow(clippy::never_loop)] // All branches return by design — the loop is for future inline tool execution
    pub async fn run(
        &self,
        mut ctx: AgentContext,
        user_message: &str,
    ) -> Result<AgentRunResult, AgentError> {
        let now = now_epoch();
        let user_msg = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: user_message.to_string(),
            }],
            created_at: now,
        };

        ctx.messages.push(user_msg.clone());

        let mut new_messages = vec![user_msg];

        for _ in 0..self.max_iterations {
            let response = self
                .llm
                .send_message(&ctx.system_prompt, &ctx.messages, &ctx.tools)
                .await?;

            let assistant_msg = Message {
                role: Role::Assistant,
                content: response.content.clone(),
                created_at: now_epoch(),
            };

            ctx.messages.push(assistant_msg.clone());
            new_messages.push(assistant_msg);

            match response.stop_reason {
                StopReason::EndTurn => {
                    let answer = response.content.iter().find_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    });
                    return Ok(AgentRunResult {
                        new_messages,
                        answer,
                        pending_tool_calls: vec![],
                    });
                }
                StopReason::ToolUse => {
                    let tool_calls: Vec<ToolCall> = response
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolUse { id, name, input } => Some(ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            }),
                            _ => None,
                        })
                        .collect();

                    return Ok(AgentRunResult {
                        new_messages,
                        answer: None,
                        pending_tool_calls: tool_calls,
                    });
                }
                StopReason::MaxTokens => {
                    return Err(AgentError::UnexpectedStopReason("max_tokens".to_string()));
                }
                StopReason::StopSequence => {
                    return Err(AgentError::UnexpectedStopReason(
                        "stop_sequence".to_string(),
                    ));
                }
            }
        }

        Err(AgentError::MaxIterationsExceeded(self.max_iterations))
    }

    #[allow(clippy::never_loop)] // Same as run() — returns to DO at every tool-call boundary
    pub async fn resume(
        &self,
        mut ctx: AgentContext,
        tool_results: Vec<ToolResult>,
    ) -> Result<AgentRunResult, AgentError> {
        let tool_result_msg = Message {
            role: Role::User,
            content: tool_results
                .into_iter()
                .map(|r| ContentBlock::ToolResult {
                    tool_use_id: r.tool_use_id,
                    content: r.content,
                    is_error: r.is_error,
                })
                .collect(),
            created_at: now_epoch(),
        };

        ctx.messages.push(tool_result_msg.clone());

        let mut new_messages = vec![tool_result_msg];

        for _ in 0..self.max_iterations {
            let response = self
                .llm
                .send_message(&ctx.system_prompt, &ctx.messages, &ctx.tools)
                .await?;

            let assistant_msg = Message {
                role: Role::Assistant,
                content: response.content.clone(),
                created_at: now_epoch(),
            };

            ctx.messages.push(assistant_msg.clone());
            new_messages.push(assistant_msg);

            match response.stop_reason {
                StopReason::EndTurn => {
                    let answer = response.content.iter().find_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    });
                    return Ok(AgentRunResult {
                        new_messages,
                        answer,
                        pending_tool_calls: vec![],
                    });
                }
                StopReason::ToolUse => {
                    let tool_calls: Vec<ToolCall> = response
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolUse { id, name, input } => Some(ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            }),
                            _ => None,
                        })
                        .collect();

                    return Ok(AgentRunResult {
                        new_messages,
                        answer: None,
                        pending_tool_calls: tool_calls,
                    });
                }
                StopReason::MaxTokens => {
                    return Err(AgentError::UnexpectedStopReason("max_tokens".to_string()));
                }
                StopReason::StopSequence => {
                    return Err(AgentError::UnexpectedStopReason(
                        "stop_sequence".to_string(),
                    ));
                }
            }
        }

        Err(AgentError::MaxIterationsExceeded(self.max_iterations))
    }
}

fn now_epoch() -> i64 {
    // In WASM, Date.now() is available via js_sys, but agent-core has no
    // JS dependency. The caller (AgentDO) can overwrite created_at if needed.
    // For now return 0 as a placeholder — the DO layer will set real timestamps.
    0
}
