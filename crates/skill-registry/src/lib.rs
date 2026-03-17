use agent_core::{AgentError, HttpBackend, ToolCall, ToolDefinition, ToolExecutor, ToolResult};
use async_trait::async_trait;
use mcp_client::McpClient;
use serde::{Deserialize, Serialize};

/// Data transfer type matching the SQLite `skills` table schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRow {
    pub name: String,
    pub url: String,
    pub tools_json: String,
    pub added_at: i64,
    #[serde(default)]
    pub auth_header_name: Option<String>,
    #[serde(default)]
    pub auth_header_value: Option<String>,
}

fn build_extra_headers(
    auth_header_name: Option<&str>,
    auth_header_value: Option<&str>,
) -> Vec<(String, String)> {
    match auth_header_value {
        Some(value) => {
            let name = auth_header_name.unwrap_or("authorization").to_string();
            vec![(name, value.to_string())]
        }
        None => vec![],
    }
}

struct RegisteredSkill<H: HttpBackend> {
    name: String,
    tools: Vec<ToolDefinition>,
    client: McpClient<H>,
}

pub struct SkillRegistry<H: HttpBackend> {
    skills: Vec<RegisteredSkill<H>>,
}

impl<H: HttpBackend> SkillRegistry<H> {
    /// Build a registry from cached SQLite rows without making any network calls.
    /// `backend_factory` creates an HttpBackend for each skill URL.
    pub fn from_rows(
        rows: Vec<SkillRow>,
        backend_factory: impl Fn() -> H,
    ) -> Result<Self, AgentError> {
        let mut skills = Vec::with_capacity(rows.len());
        for row in rows {
            let tools: Vec<ToolDefinition> =
                serde_json::from_str(&row.tools_json).map_err(|e| {
                    AgentError::McpError(format!("Bad cached tools for {}: {e}", row.name))
                })?;
            let extra_headers = build_extra_headers(
                row.auth_header_name.as_deref(),
                row.auth_header_value.as_deref(),
            );
            let client = McpClient::new(backend_factory(), row.url, extra_headers);
            skills.push(RegisteredSkill {
                name: row.name,
                tools,
                client,
            });
        }
        Ok(Self { skills })
    }

    /// Register a new skill: connect, initialize, list tools, return a SkillRow
    /// suitable for persisting to SQLite.
    pub async fn register(
        &mut self,
        name: String,
        url: String,
        backend: H,
        now: i64,
        auth_header_name: Option<String>,
        auth_header_value: Option<String>,
    ) -> Result<SkillRow, AgentError> {
        let extra_headers =
            build_extra_headers(auth_header_name.as_deref(), auth_header_value.as_deref());
        let client = McpClient::new(backend, url.clone(), extra_headers);

        // Initialize the MCP connection
        client.initialize().await?;

        // Discover tools
        let tools = client.list_tools().await?;
        let tools_json = serde_json::to_string(&tools).map_err(AgentError::Serialization)?;

        let row = SkillRow {
            name: name.clone(),
            url,
            tools_json,
            added_at: now,
            auth_header_name,
            auth_header_value,
        };

        self.skills.push(RegisteredSkill {
            name,
            tools,
            client,
        });

        Ok(row)
    }

    /// Returns the union of all tool definitions, namespaced as `"skill_name__tool_name"`.
    /// Uses `__` separator because Anthropic tool names must match `^[a-zA-Z0-9_-]{1,128}$`.
    pub fn all_tools(&self) -> Vec<ToolDefinition> {
        let mut all = Vec::new();
        for skill in &self.skills {
            for tool in &skill.tools {
                all.push(ToolDefinition {
                    name: format!("{}__{}", skill.name, tool.name),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                });
            }
        }
        all
    }

    /// Route a namespaced tool call to the correct MCP client.
    /// Splits on the first `__` to find the skill name.
    pub async fn dispatch(&self, tool_call: &ToolCall) -> Result<ToolResult, AgentError> {
        let (skill_name, tool_name) = tool_call.name.split_once("__").ok_or_else(|| {
            AgentError::SkillNotFound(format!(
                "Tool name '{}' has no skill prefix (expected 'skill__tool')",
                tool_call.name
            ))
        })?;

        let skill = self
            .skills
            .iter()
            .find(|s| s.name == skill_name)
            .ok_or_else(|| AgentError::SkillNotFound(skill_name.to_string()))?;

        let inner_call = ToolCall {
            id: tool_call.id.clone(),
            name: tool_name.to_string(),
            input: tool_call.input.clone(),
        };

        let mut result = skill
            .client
            .call_tool(tool_name, tool_call.input.clone())
            .await?;
        result.tool_use_id = inner_call.id;
        Ok(result)
    }
}

#[async_trait(?Send)]
impl<H: HttpBackend> ToolExecutor for SkillRegistry<H> {
    async fn execute(&self, tool_call: &ToolCall) -> Result<ToolResult, AgentError> {
        self.dispatch(tool_call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::HttpBackend;
    use async_trait::async_trait;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    struct MockHttpBackend {
        responses: RefCell<VecDeque<Vec<u8>>>,
        captured_headers: Rc<RefCell<Vec<Vec<(String, String)>>>>,
    }

    impl MockHttpBackend {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: RefCell::new(
                    responses
                        .into_iter()
                        .map(|s| s.as_bytes().to_vec())
                        .collect(),
                ),
                captured_headers: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn empty() -> Self {
            Self::new(vec![])
        }
    }

    #[async_trait(?Send)]
    impl HttpBackend for MockHttpBackend {
        async fn post(
            &self,
            _url: &str,
            headers: &[(&str, &str)],
            _body: &[u8],
        ) -> Result<Vec<u8>, AgentError> {
            self.captured_headers.borrow_mut().push(
                headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            );
            self.responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| AgentError::Http("No more mock responses".to_string()))
        }
    }

    fn sample_tool_def() -> ToolDefinition {
        ToolDefinition {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        }
    }

    #[test]
    fn test_from_rows_and_all_tools() {
        let tools = vec![sample_tool_def()];
        let tools_json = serde_json::to_string(&tools).unwrap();

        let rows = vec![SkillRow {
            name: "websearch".to_string(),
            url: "http://localhost:8787".to_string(),
            tools_json,
            added_at: 1000,
            auth_header_name: None,
            auth_header_value: None,
        }];

        let registry = SkillRegistry::from_rows(rows, MockHttpBackend::empty).unwrap();
        let all = registry.all_tools();

        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "websearch__search");
        assert_eq!(all[0].description, "Search the web");
    }

    #[test]
    fn test_namespace_multiple_skills() {
        let tool_a = serde_json::to_string(&vec![ToolDefinition {
            name: "store".to_string(),
            description: "Store a memory".to_string(),
            input_schema: serde_json::json!({}),
        }])
        .unwrap();

        let tool_b = serde_json::to_string(&vec![ToolDefinition {
            name: "fetch".to_string(),
            description: "Fetch a URL".to_string(),
            input_schema: serde_json::json!({}),
        }])
        .unwrap();

        let rows = vec![
            SkillRow {
                name: "memory".to_string(),
                url: "http://mem:8787".to_string(),
                tools_json: tool_a,
                added_at: 1000,
                auth_header_name: None,
                auth_header_value: None,
            },
            SkillRow {
                name: "http".to_string(),
                url: "http://http:8787".to_string(),
                tools_json: tool_b,
                added_at: 1000,
                auth_header_name: None,
                auth_header_value: None,
            },
        ];

        let registry = SkillRegistry::from_rows(rows, MockHttpBackend::empty).unwrap();
        let all = registry.all_tools();

        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "memory__store");
        assert_eq!(all[1].name, "http__fetch");
    }

    #[tokio::test]
    async fn test_dispatch_routes_correctly() {
        let call_response = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"search results"}],"is_error":false}}"#;

        let tools = vec![sample_tool_def()];
        let tools_json = serde_json::to_string(&tools).unwrap();

        let rows = vec![SkillRow {
            name: "websearch".to_string(),
            url: "http://localhost:8787".to_string(),
            tools_json,
            added_at: 1000,
            auth_header_name: None,
            auth_header_value: None,
        }];

        let registry =
            SkillRegistry::from_rows(rows, || MockHttpBackend::new(vec![call_response])).unwrap();

        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "websearch__search".to_string(),
            input: serde_json::json!({"query": "test"}),
        };

        let result = registry.dispatch(&tool_call).await.unwrap();
        assert_eq!(result.tool_use_id, "call_1");
        assert_eq!(result.content, "search results");
    }

    #[tokio::test]
    async fn test_dispatch_unknown_skill() {
        let registry =
            SkillRegistry::<MockHttpBackend>::from_rows(vec![], MockHttpBackend::empty).unwrap();

        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "unknown__tool".to_string(),
            input: serde_json::json!({}),
        };

        let err = registry.dispatch(&tool_call).await.unwrap_err();
        assert!(matches!(err, AgentError::SkillNotFound(_)));
    }

    #[tokio::test]
    async fn test_dispatch_no_prefix() {
        let registry =
            SkillRegistry::<MockHttpBackend>::from_rows(vec![], MockHttpBackend::empty).unwrap();

        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "no_prefix".to_string(),
            input: serde_json::json!({}),
        };

        let err = registry.dispatch(&tool_call).await.unwrap_err();
        assert!(matches!(err, AgentError::SkillNotFound(_)));
    }

    #[tokio::test]
    async fn test_register_skill() {
        let init_response = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1.0"}}}"#;
        let tools_response = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"search","description":"Search","inputSchema":{"type":"object"}}]}}"#;

        let mut registry = SkillRegistry::from_rows(vec![], MockHttpBackend::empty).unwrap();

        let row = registry
            .register(
                "websearch".to_string(),
                "http://localhost:8787".to_string(),
                MockHttpBackend::new(vec![init_response, tools_response]),
                12345,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(row.name, "websearch");
        assert_eq!(row.added_at, 12345);

        let all = registry.all_tools();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "websearch__search");
    }

    #[tokio::test]
    async fn test_from_rows_with_auth_headers() {
        let call_response = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"ok"}],"is_error":false}}"#;

        let tools = vec![sample_tool_def()];
        let tools_json = serde_json::to_string(&tools).unwrap();

        let rows = vec![SkillRow {
            name: "authed".to_string(),
            url: "http://localhost:8787".to_string(),
            tools_json,
            added_at: 1000,
            auth_header_name: Some("x-api-key".to_string()),
            auth_header_value: Some("secret-key-123".to_string()),
        }];

        let captured = Rc::new(RefCell::new(Vec::<Vec<(String, String)>>::new()));
        let captured_for_factory = captured.clone();

        let registry = SkillRegistry::from_rows(rows, move || {
            let responses: VecDeque<Vec<u8>> = vec![call_response.as_bytes().to_vec()]
                .into_iter()
                .collect();
            MockHttpBackend {
                responses: RefCell::new(responses),
                captured_headers: captured_for_factory.clone(),
            }
        })
        .unwrap();

        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "authed__search".to_string(),
            input: serde_json::json!({"query": "test"}),
        };

        let result = registry.dispatch(&tool_call).await.unwrap();
        assert_eq!(result.content, "ok");

        let headers = captured.borrow();
        assert_eq!(headers.len(), 1);
        assert!(headers[0]
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "secret-key-123"));
    }
}
