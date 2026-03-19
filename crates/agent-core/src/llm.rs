use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::AgentError;
use crate::types::{ContentBlock, Message, ToolDefinition};

#[cfg(feature = "native")]
#[async_trait]
pub trait HttpBackend: Send + Sync {
    async fn post(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<u8>, AgentError>;
}

#[cfg(not(feature = "native"))]
#[async_trait(?Send)]
pub trait HttpBackend {
    async fn post(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<u8>, AgentError>;
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub max_tokens: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "claude-sonnet-4-20250514".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            max_tokens: 4096,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub stop_reason: StopReason,
    pub content: Vec<ContentBlock>,
}

pub struct LlmClient<H: HttpBackend> {
    pub config: LlmConfig,
    pub backend: H,
}

// --- Anthropic API request/response wire types ---

#[derive(Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<ApiMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiTool<'a>>,
}

#[derive(Serialize)]
struct ApiMessage<'a> {
    role: &'a str,
    content: &'a Vec<ContentBlock>,
}

#[derive(Serialize)]
struct ApiTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
    stop_reason: StopReason,
}

#[derive(Deserialize)]
struct ApiError {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    message: String,
}

impl<H: HttpBackend> LlmClient<H> {
    pub fn new(config: LlmConfig, backend: H) -> Self {
        Self { config, backend }
    }

    pub async fn send_message(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, AgentError> {
        let api_messages: Vec<ApiMessage> = messages
            .iter()
            .map(|m| ApiMessage {
                role: match m.role {
                    crate::types::Role::User => "user",
                    crate::types::Role::Assistant => "assistant",
                },
                content: &m.content,
            })
            .collect();

        let api_tools: Vec<ApiTool> = tools
            .iter()
            .map(|t| ApiTool {
                name: &t.name,
                description: &t.description,
                input_schema: &t.input_schema,
            })
            .collect();

        let request = ApiRequest {
            model: &self.config.model,
            max_tokens: self.config.max_tokens,
            system,
            messages: api_messages,
            tools: api_tools,
        };

        let body = serde_json::to_vec(&request)?;
        let url = format!("{}/v1/messages", self.config.base_url);

        let headers = [
            ("content-type", "application/json"),
            ("x-api-key", &self.config.api_key),
            ("anthropic-version", "2023-06-01"),
        ];

        let response_bytes = self.backend.post(&url, &headers, &body).await?;

        // Try success first (has required `content` + `stop_reason` fields),
        // then fall back to error parsing. This avoids false positives from
        // lenient deserialization of ApiError against valid responses.
        let api_response: ApiResponse = match serde_json::from_slice(&response_bytes) {
            Ok(resp) => resp,
            Err(_) => {
                if let Ok(api_error) = serde_json::from_slice::<ApiError>(&response_bytes) {
                    return Err(AgentError::LlmRequestFailed(api_error.error.message));
                }
                return Err(AgentError::LlmResponseParse(
                    String::from_utf8_lossy(&response_bytes).into_owned(),
                ));
            }
        };

        Ok(LlmResponse {
            stop_reason: api_response.stop_reason,
            content: api_response.content,
        })
    }
}

#[cfg(feature = "native")]
pub struct ReqwestBackend {
    client: reqwest::Client,
}

#[cfg(feature = "native")]
impl ReqwestBackend {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[cfg(feature = "native")]
impl Default for ReqwestBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "native")]
#[async_trait]
impl HttpBackend for ReqwestBackend {
    async fn post(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<u8>, AgentError> {
        let mut builder = self.client.post(url).body(body.to_vec());
        for (key, value) in headers {
            builder = builder.header(*key, *value);
        }
        let response = builder
            .send()
            .await
            .map_err(|e| AgentError::Http(e.to_string()))?;
        let bytes = response
            .bytes()
            .await
            .map_err(|e| AgentError::Http(e.to_string()))?;
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;
    use std::sync::Mutex;

    struct MockBackend {
        response: Mutex<Option<Vec<u8>>>,
    }

    impl MockBackend {
        fn new(response: &str) -> Self {
            Self {
                response: Mutex::new(Some(response.as_bytes().to_vec())),
            }
        }
    }

    #[cfg_attr(feature = "native", async_trait)]
    #[cfg_attr(not(feature = "native"), async_trait(?Send))]
    impl HttpBackend for MockBackend {
        async fn post(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            _body: &[u8],
        ) -> Result<Vec<u8>, AgentError> {
            self.response
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| AgentError::Http("No response".to_string()))
        }
    }

    fn make_client(response: &str) -> LlmClient<MockBackend> {
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            ..LlmConfig::default()
        };
        LlmClient::new(config, MockBackend::new(response))
    }

    #[tokio::test]
    async fn test_send_message_success() {
        let fixture = include_str!("../../../tests/fixtures/end_turn_response.json");
        let client = make_client(fixture);
        let result = client.send_message("system", &[], &[]).await.unwrap();

        assert!(matches!(result.stop_reason, StopReason::EndTurn));
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            ContentBlock::Text { text } => {
                assert_eq!(text, "Hello! How can I help you today?");
            }
            _ => panic!("Expected Text content block"),
        }
    }

    #[tokio::test]
    async fn test_send_message_api_error() {
        let fixture = include_str!("../../../tests/fixtures/api_error_response.json");
        let client = make_client(fixture);
        let err = client.send_message("system", &[], &[]).await.unwrap_err();

        assert!(
            matches!(err, AgentError::LlmRequestFailed(ref msg) if msg.contains("credit balance"))
        );
    }

    #[tokio::test]
    async fn test_send_message_invalid_json() {
        let client = make_client("not valid json at all");
        let err = client.send_message("system", &[], &[]).await.unwrap_err();
        assert!(matches!(err, AgentError::LlmResponseParse(_)));
    }
}
