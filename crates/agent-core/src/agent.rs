use std::sync::Arc;

use crate::error::AgentError;
use crate::llm::{HttpBackend, LlmClient, StopReason};
use crate::types::{
    AgentContext, AgentRunResult, ContentBlock, Message, Role, ToolCall, ToolExecutor, ToolResult,
};

pub struct Agent<H: HttpBackend> {
    pub llm: LlmClient<H>,
    pub max_iterations: u32,
    pub max_continuations: u32,
    pub tool_executor: Option<Arc<dyn ToolExecutor>>,
}

impl<H: HttpBackend> Agent<H> {
    pub fn new(llm: LlmClient<H>) -> Self {
        Self {
            llm,
            max_iterations: 10,
            max_continuations: 3,
            tool_executor: None,
        }
    }

    pub fn with_tool_executor(mut self, executor: Arc<dyn ToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    pub fn with_max_continuations(mut self, n: u32) -> Self {
        self.max_continuations = n;
        self
    }

    pub async fn run(
        &self,
        mut ctx: AgentContext,
        user_message: &str,
    ) -> Result<AgentRunResult, AgentError> {
        let user_msg = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: user_message.to_string(),
            }],
            created_at: now_epoch(),
        };

        ctx.messages.push(user_msg.clone());
        let new_messages = vec![user_msg];

        self.agent_loop(&mut ctx, new_messages).await
    }

    pub async fn resume(
        &self,
        mut ctx: AgentContext,
        tool_results: Vec<ToolResult>,
    ) -> Result<AgentRunResult, AgentError> {
        let tool_result_msg = Message {
            role: Role::User,
            content: tool_results.into_iter().map(ContentBlock::from).collect(),
            created_at: now_epoch(),
        };

        ctx.messages.push(tool_result_msg.clone());
        let new_messages = vec![tool_result_msg];

        self.agent_loop(&mut ctx, new_messages).await
    }

    async fn agent_loop(
        &self,
        ctx: &mut AgentContext,
        mut new_messages: Vec<Message>,
    ) -> Result<AgentRunResult, AgentError> {
        let mut continuations_used: u32 = 0;

        for _ in 0..self.max_iterations {
            let response = match self
                .llm
                .send_message(&ctx.system_prompt, &ctx.messages, &ctx.tools)
                .await
            {
                Ok(resp) => resp,
                Err(AgentError::LlmRequestFailed(ref msg))
                    if msg.contains("prompt is too long") =>
                {
                    return Err(AgentError::PromptTooLong);
                }
                Err(e) => return Err(e),
            };

            let assistant_msg = Message {
                role: Role::Assistant,
                content: response.content,
                created_at: now_epoch(),
            };

            ctx.messages.push(assistant_msg.clone());
            new_messages.push(assistant_msg);

            // Extract answer/tool_calls from the last message pushed (avoids re-iterating response.content)
            let last_content = &ctx.messages.last().unwrap().content;

            match response.stop_reason {
                StopReason::EndTurn => {
                    let answer = last_content.iter().find_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    });
                    return Ok(AgentRunResult {
                        new_messages,
                        answer,
                        pending_tool_calls: vec![],
                    });
                }

                StopReason::MaxTokens => {
                    if continuations_used >= self.max_continuations {
                        return Err(AgentError::MaxContinuationsExceeded(self.max_continuations));
                    }
                    continuations_used += 1;
                    let continue_msg = Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "Continue from where you left off.".to_string(),
                        }],
                        created_at: now_epoch(),
                    };
                    ctx.messages.push(continue_msg.clone());
                    new_messages.push(continue_msg);
                    continue;
                }

                StopReason::ToolUse => {
                    let tool_calls: Vec<ToolCall> = last_content
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

                    let executor = match &self.tool_executor {
                        None => {
                            // No executor: return all tool calls to the harness
                            return Ok(AgentRunResult {
                                new_messages,
                                answer: None,
                                pending_tool_calls: tool_calls,
                            });
                        }
                        Some(e) => e.clone(),
                    };

                    // Execute ALL tools unconditionally. Permission checks
                    // are the harness's responsibility, not the agent loop's.
                    let results = execute_tools(&executor, tool_calls).await;

                    let tool_result_msg = Message {
                        role: Role::User,
                        content: results.into_iter().map(ContentBlock::from).collect(),
                        created_at: now_epoch(),
                    };
                    ctx.messages.push(tool_result_msg.clone());
                    new_messages.push(tool_result_msg);

                    continue;
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

async fn execute_tools(
    executor: &Arc<dyn ToolExecutor>,
    tool_calls: Vec<ToolCall>,
) -> Vec<ToolResult> {
    let (concurrent, serial): (Vec<_>, Vec<_>) = tool_calls
        .into_iter()
        .partition(|tc| executor.is_concurrent_safe(tc));

    let mut results = Vec::new();

    if !concurrent.is_empty() {
        let futs: Vec<_> = concurrent
            .into_iter()
            .map(|tc| {
                let executor = executor.clone();
                async move {
                    let id = tc.id.clone();
                    executor
                        .execute(&tc)
                        .await
                        .unwrap_or_else(|e| ToolResult::error_for(id, e))
                }
            })
            .collect();
        results.extend(futures_util::future::join_all(futs).await);
    }

    for tc in &serial {
        let result = executor
            .execute(tc)
            .await
            .unwrap_or_else(|e| ToolResult::error_for(tc.id.clone(), e));
        results.push(result);
    }

    results
}

fn now_epoch() -> i64 {
    // Placeholder for WASM compat — server layer overwrites with real timestamps.
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{HttpBackend, LlmClient, LlmConfig};
    use async_trait::async_trait;
    use std::collections::{HashSet, VecDeque};
    use std::sync::Mutex;

    // --- Mock HTTP Backend ---

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
        ) -> Result<crate::llm::HttpResponse, AgentError> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .map(crate::llm::HttpResponse::body_only)
                .ok_or_else(|| AgentError::Http("No more mock responses".to_string()))
        }
    }

    // --- Mock Tool Executor ---

    struct MockToolExecutor {
        concurrent_tools: HashSet<String>,
        call_log: Mutex<Vec<ToolCall>>,
        error_tools: HashSet<String>,
    }

    #[allow(dead_code)]
    impl MockToolExecutor {
        fn new() -> Self {
            Self {
                concurrent_tools: HashSet::new(),
                call_log: Mutex::new(Vec::new()),
                error_tools: HashSet::new(),
            }
        }

        fn with_concurrent_tools(mut self, tools: Vec<&str>) -> Self {
            self.concurrent_tools = tools.into_iter().map(String::from).collect();
            self
        }

        fn with_error_tools(mut self, tools: Vec<&str>) -> Self {
            self.error_tools = tools.into_iter().map(String::from).collect();
            self
        }

        fn calls(&self) -> Vec<ToolCall> {
            self.call_log.lock().unwrap().clone()
        }
    }

    #[cfg_attr(feature = "native", async_trait)]
    #[cfg_attr(not(feature = "native"), async_trait(?Send))]
    impl ToolExecutor for MockToolExecutor {
        async fn execute(&self, tool_call: &ToolCall) -> Result<ToolResult, AgentError> {
            self.call_log.lock().unwrap().push(tool_call.clone());
            if self.error_tools.contains(&tool_call.name) {
                return Err(AgentError::ToolExecutionFailed(format!(
                    "{} failed",
                    tool_call.name
                )));
            }
            Ok(ToolResult {
                tool_use_id: tool_call.id.clone(),
                content: format!("Result for {}", tool_call.name),
                is_error: false,
            })
        }

        fn is_concurrent_safe(&self, tool_call: &ToolCall) -> bool {
            self.concurrent_tools.contains(&tool_call.name)
        }
    }

    // --- Helpers ---

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

    fn make_agent_with_executor(
        responses: Vec<&str>,
        executor: MockToolExecutor,
    ) -> Agent<MockHttpBackend> {
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
        Agent::new(LlmClient::new(config, backend)).with_tool_executor(Arc::new(executor))
    }

    fn empty_ctx() -> AgentContext {
        AgentContext {
            system_prompt: "You are helpful.".to_string(),
            messages: vec![],
            tools: vec![],
        }
    }

    // --- Original tests (preserved) ---

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
        // With max_continuations=0, MaxTokens immediately errors
        let response =
            r#"{"content":[{"type":"text","text":"partial"}],"stop_reason":"max_tokens"}"#;
        let backend = MockHttpBackend::new(vec![response.as_bytes().to_vec()]);
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            ..LlmConfig::default()
        };
        let agent = Agent::new(LlmClient::new(config, backend)).with_max_continuations(0);
        let err = agent.run(empty_ctx(), "Hi").await.unwrap_err();
        assert!(matches!(err, AgentError::MaxContinuationsExceeded(0)));
    }

    // --- Sub-milestone B: Inline execution tests ---

    #[tokio::test]
    async fn test_run_no_executor_legacy() {
        // Without executor, ToolUse returns pending_tool_calls (backward compat)
        let fixture = include_str!("../../../tests/fixtures/tool_use_response.json");
        let agent = make_agent(vec![fixture]);
        let result = agent.run(empty_ctx(), "Search").await.unwrap();

        assert!(result.answer.is_none());
        assert_eq!(result.pending_tool_calls.len(), 1);
        assert_eq!(result.pending_tool_calls[0].name, "web_search");
    }

    #[tokio::test]
    async fn test_run_inline_safe_tool() {
        let tool_use = include_str!("../../../tests/fixtures/tool_use_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        let executor = MockToolExecutor::new();

        let agent = make_agent_with_executor(vec![tool_use, end_turn], executor);
        let result = agent.run(empty_ctx(), "Search").await.unwrap();

        // Should get answer (tool was executed inline, then LLM gave EndTurn)
        assert_eq!(
            result.answer,
            Some("Hello! How can I help you today?".to_string())
        );
        assert!(result.pending_tool_calls.is_empty());
        // Messages: user, assistant(tool_use), user(tool_result), assistant(end_turn)
        assert_eq!(result.new_messages.len(), 4);
    }

    #[tokio::test]
    async fn test_run_multi_iteration_tools() {
        let tool_use = include_str!("../../../tests/fixtures/tool_use_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        let executor = MockToolExecutor::new();

        // ToolUse → ToolUse → EndTurn (3 LLM calls, 2 tool executions)
        let agent = make_agent_with_executor(vec![tool_use, tool_use, end_turn], executor);
        let result = agent.run(empty_ctx(), "Do two things").await.unwrap();

        assert_eq!(
            result.answer,
            Some("Hello! How can I help you today?".to_string())
        );
        assert!(result.pending_tool_calls.is_empty());
        // user, asst(tool_use), user(tool_result), asst(tool_use), user(tool_result), asst(end_turn)
        assert_eq!(result.new_messages.len(), 6);
    }

    #[tokio::test]
    async fn test_max_iterations_with_executor() {
        let tool_use = include_str!("../../../tests/fixtures/tool_use_response.json");
        let executor = MockToolExecutor::new();

        // All responses are ToolUse — should hit max_iterations
        let responses: Vec<&str> = vec![tool_use; 10];
        let agent = make_agent_with_executor(responses, executor);
        let err = agent.run(empty_ctx(), "Loop forever").await.unwrap_err();

        assert!(matches!(err, AgentError::MaxIterationsExceeded(10)));
    }

    #[tokio::test]
    async fn test_resume_with_executor() {
        let tool_use = include_str!("../../../tests/fixtures/tool_use_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        let executor = MockToolExecutor::new();

        // resume → ToolUse (executed inline) → EndTurn
        let agent = make_agent_with_executor(vec![tool_use, end_turn], executor);

        let tool_results = vec![ToolResult {
            tool_use_id: "toolu_prev".to_string(),
            content: "previous result".to_string(),
            is_error: false,
        }];

        let result = agent.resume(empty_ctx(), tool_results).await.unwrap();
        assert_eq!(
            result.answer,
            Some("Hello! How can I help you today?".to_string())
        );
        assert!(result.pending_tool_calls.is_empty());
    }

    // --- Sub-milestone D: Error recovery tests ---

    #[tokio::test]
    async fn test_max_tokens_continues() {
        let max_tokens = include_str!("../../../tests/fixtures/max_tokens_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");

        let agent = make_agent(vec![max_tokens, end_turn]);
        let result = agent.run(empty_ctx(), "Hi").await.unwrap();

        // Should auto-continue and get the final answer
        assert_eq!(
            result.answer,
            Some("Hello! How can I help you today?".to_string())
        );
        // user, asst(max_tokens), user(continue), asst(end_turn)
        assert_eq!(result.new_messages.len(), 4);

        // Verify the continuation message
        let continue_msg = &result.new_messages[2];
        assert_eq!(continue_msg.role, Role::User);
        match &continue_msg.content[0] {
            ContentBlock::Text { text } => {
                assert_eq!(text, "Continue from where you left off.");
            }
            _ => panic!("Expected Text"),
        }
    }

    #[tokio::test]
    async fn test_max_continuations_exceeded() {
        let max_tokens =
            r#"{"content":[{"type":"text","text":"partial"}],"stop_reason":"max_tokens"}"#;

        // 3 LLM calls: original + 2 continuations, all return max_tokens
        let backend = MockHttpBackend::new(vec![
            max_tokens.as_bytes().to_vec(),
            max_tokens.as_bytes().to_vec(),
            max_tokens.as_bytes().to_vec(),
        ]);
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            ..LlmConfig::default()
        };
        let agent = Agent::new(LlmClient::new(config, backend)).with_max_continuations(2);
        let err = agent.run(empty_ctx(), "Hi").await.unwrap_err();
        assert!(matches!(err, AgentError::MaxContinuationsExceeded(2)));
    }

    #[tokio::test]
    async fn test_prompt_too_long_error() {
        let error_response = include_str!("../../../tests/fixtures/prompt_too_long_error.json");

        let agent = make_agent(vec![error_response]);
        let err = agent.run(empty_ctx(), "Hi").await.unwrap_err();
        assert!(matches!(err, AgentError::PromptTooLong));
    }

    #[tokio::test]
    async fn test_max_tokens_then_tool_use() {
        let max_tokens = include_str!("../../../tests/fixtures/max_tokens_response.json");
        let tool_use = include_str!("../../../tests/fixtures/tool_use_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        let executor = MockToolExecutor::new();

        // MaxTokens → continue → ToolUse (inline) → EndTurn
        let agent = make_agent_with_executor(vec![max_tokens, tool_use, end_turn], executor);
        let result = agent.run(empty_ctx(), "Hi").await.unwrap();

        assert_eq!(
            result.answer,
            Some("Hello! How can I help you today?".to_string())
        );
        assert!(result.pending_tool_calls.is_empty());
    }

    #[tokio::test]
    async fn test_max_continuations_zero_immediate_error() {
        let max_tokens =
            r#"{"content":[{"type":"text","text":"partial"}],"stop_reason":"max_tokens"}"#;
        let backend = MockHttpBackend::new(vec![max_tokens.as_bytes().to_vec()]);
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            ..LlmConfig::default()
        };
        let agent = Agent::new(LlmClient::new(config, backend)).with_max_continuations(0);
        let err = agent.run(empty_ctx(), "Hi").await.unwrap_err();
        assert!(matches!(err, AgentError::MaxContinuationsExceeded(0)));
    }

    // --- Sub-milestone C: Concurrent execution tests ---

    #[tokio::test]
    async fn test_concurrent_tools_both_called() {
        let multi_tool = include_str!("../../../tests/fixtures/multi_tool_use_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        // Both tools marked concurrent-safe
        let executor =
            MockToolExecutor::new().with_concurrent_tools(vec!["web_search", "http_fetch"]);

        let agent = make_agent_with_executor(vec![multi_tool, end_turn], executor);
        let result = agent.run(empty_ctx(), "Search and fetch").await.unwrap();

        assert!(result.answer.is_some());
        assert!(result.pending_tool_calls.is_empty());
        // user, asst(tool_use x2), user(tool_result x2), asst(end_turn)
        assert_eq!(result.new_messages.len(), 4);

        // Verify both tool results are in the message
        let tool_result_msg = &result.new_messages[2];
        assert_eq!(tool_result_msg.content.len(), 2);
    }

    #[tokio::test]
    async fn test_mixed_concurrent_and_serial() {
        let multi_tool = include_str!("../../../tests/fixtures/multi_tool_use_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        // Only web_search is concurrent, http_fetch is serial
        let executor = MockToolExecutor::new().with_concurrent_tools(vec!["web_search"]);

        let agent = make_agent_with_executor(vec![multi_tool, end_turn], executor);
        let result = agent.run(empty_ctx(), "Search and fetch").await.unwrap();

        assert!(result.answer.is_some());
        // Both tools should have been executed
        let tool_result_msg = &result.new_messages[2];
        assert_eq!(tool_result_msg.content.len(), 2);
    }

    #[tokio::test]
    async fn test_all_serial_preserved_order() {
        let multi_tool = include_str!("../../../tests/fixtures/multi_tool_use_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        // No concurrent tools — all serial
        let executor = MockToolExecutor::new();

        let agent = make_agent_with_executor(vec![multi_tool, end_turn], executor);
        let result = agent.run(empty_ctx(), "Do things").await.unwrap();

        assert!(result.answer.is_some());
        let tool_result_msg = &result.new_messages[2];
        assert_eq!(tool_result_msg.content.len(), 2);

        // Verify order matches input order (web_search first, http_fetch second)
        match &tool_result_msg.content[0] {
            ContentBlock::ToolResult { tool_use_id, .. } => {
                assert_eq!(tool_use_id, "toolu_01A");
            }
            _ => panic!("Expected ToolResult"),
        }
        match &tool_result_msg.content[1] {
            ContentBlock::ToolResult { tool_use_id, .. } => {
                assert_eq!(tool_use_id, "toolu_01B");
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[tokio::test]
    async fn test_concurrent_tool_error_isolated() {
        let multi_tool = include_str!("../../../tests/fixtures/multi_tool_use_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        // web_search errors, http_fetch succeeds — both concurrent
        let executor = MockToolExecutor::new()
            .with_concurrent_tools(vec!["web_search", "http_fetch"])
            .with_error_tools(vec!["web_search"]);

        let agent = make_agent_with_executor(vec![multi_tool, end_turn], executor);
        let result = agent.run(empty_ctx(), "Search and fetch").await.unwrap();

        // Agent should still succeed overall
        assert!(result.answer.is_some());

        let tool_result_msg = &result.new_messages[2];
        assert_eq!(tool_result_msg.content.len(), 2);

        // Find the error and success results
        let mut found_error = false;
        let mut found_success = false;
        for block in &tool_result_msg.content {
            match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    content,
                    ..
                } => {
                    if tool_use_id == "toolu_01A" {
                        // web_search should have errored
                        assert!(is_error);
                        assert!(content.contains("web_search failed"));
                        found_error = true;
                    } else if tool_use_id == "toolu_01B" {
                        // http_fetch should succeed
                        assert!(!is_error);
                        found_success = true;
                    }
                }
                _ => panic!("Expected ToolResult"),
            }
        }
        assert!(found_error, "Should have error result for web_search");
        assert!(found_success, "Should have success result for http_fetch");
    }

    // --- Sub-milestone B: Tool error test ---

    #[tokio::test]
    async fn test_tool_execution_error_becomes_is_error() {
        let tool_use = include_str!("../../../tests/fixtures/tool_use_response.json");
        let end_turn = include_str!("../../../tests/fixtures/end_turn_response.json");
        let executor = MockToolExecutor::new().with_error_tools(vec!["web_search"]);

        let agent = make_agent_with_executor(vec![tool_use, end_turn], executor);
        let result = agent.run(empty_ctx(), "Search").await.unwrap();

        // Agent should still succeed — error was passed to LLM as is_error tool result
        assert_eq!(
            result.answer,
            Some("Hello! How can I help you today?".to_string())
        );

        // Check the tool result message has is_error: true
        let tool_result_msg = &result.new_messages[2]; // user, asst, tool_result, asst
        match &tool_result_msg.content[0] {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(is_error);
                assert!(content.contains("web_search failed"));
            }
            _ => panic!("Expected ToolResult"),
        }
    }
}
