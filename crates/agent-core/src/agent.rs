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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{HttpBackend, LlmClient, LlmConfig};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct MockHttpBackend {
        responses: Mutex<VecDeque<Vec<u8>>>,
    }

    impl MockHttpBackend {
        fn new(responses: Vec<Vec<u8>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    #[cfg_attr(feature = "native", async_trait)]
    #[cfg_attr(not(feature = "native"), async_trait(?Send))]
    impl HttpBackend for MockHttpBackend {
        async fn post(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            _body: &[u8],
        ) -> Result<Vec<u8>, AgentError> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| AgentError::Http("No more mock responses".to_string()))
        }
    }

    fn make_agent(responses: Vec<&str>) -> Agent<MockHttpBackend> {
        let backend = MockHttpBackend::new(
            responses
                .into_iter()
                .map(|s| s.as_bytes().to_vec())
                .collect(),
        );
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            ..LlmConfig::default()
        };
        Agent::new(LlmClient::new(config, backend))
    }

    fn empty_ctx() -> AgentContext {
        AgentContext {
            system_prompt: "You are helpful.".to_string(),
            messages: vec![],
            tools: vec![],
        }
    }

    #[tokio::test]
    async fn test_run_end_turn() {
        let fixture = include_str!("../../../tests/fixtures/end_turn_response.json");
        let agent = make_agent(vec![fixture]);
        let result = agent.run(empty_ctx(), "Hi").await.unwrap();

        assert_eq!(
            result.answer,
            Some("Hello! How can I help you today?".to_string())
        );
        assert_eq!(result.new_messages.len(), 2); // user msg + assistant msg
        assert!(result.pending_tool_calls.is_empty());
    }

    #[tokio::test]
    async fn test_run_tool_use() {
        let fixture = include_str!("../../../tests/fixtures/tool_use_response.json");
        let agent = make_agent(vec![fixture]);
        let result = agent
            .run(empty_ctx(), "Search for Rust WASM")
            .await
            .unwrap();

        assert!(result.answer.is_none());
        assert_eq!(result.pending_tool_calls.len(), 1);
        assert_eq!(result.pending_tool_calls[0].name, "web_search");
        assert_eq!(result.pending_tool_calls[0].id, "toolu_01A");
    }

    #[tokio::test]
    async fn test_resume_after_tool_result() {
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        let tool_use = include_str!("../../../tests/fixtures/tool_use_response.json");
        let agent = make_agent(vec![tool_use, end_turn]);

        // First run triggers tool use
        let result = agent.run(empty_ctx(), "Search something").await.unwrap();
        assert!(result.answer.is_none());

        // Resume with tool results
        let mut ctx = empty_ctx();
        // Rebuild context as the DO would: prior messages + assistant tool_use msg
        ctx.messages = result.new_messages.clone();

        let tool_results = vec![ToolResult {
            tool_use_id: "toolu_01A".to_string(),
            content: "Search results here".to_string(),
            is_error: false,
        }];

        let result = agent.resume(ctx, tool_results).await.unwrap();
        assert_eq!(
            result.answer,
            Some("Hello! How can I help you today?".to_string())
        );
        assert!(result.pending_tool_calls.is_empty());
    }

    #[tokio::test]
    async fn test_run_max_tokens_error() {
        let response =
            r#"{"content":[{"type":"text","text":"partial"}],"stop_reason":"max_tokens"}"#;
        let agent = make_agent(vec![response]);
        let err = agent.run(empty_ctx(), "Hi").await.unwrap_err();
        assert!(matches!(err, AgentError::UnexpectedStopReason(ref s) if s == "max_tokens"));
    }
}
