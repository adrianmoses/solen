use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::AgentError;
use crate::types::{ContentBlock, Message, ToolDefinition};

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

        // Try to parse as success, fall back to error
        if let Ok(api_error) = serde_json::from_slice::<ApiError>(&response_bytes) {
            return Err(AgentError::LlmRequestFailed(api_error.error.message));
        }

        let api_response: ApiResponse = serde_json::from_slice(&response_bytes)
            .map_err(|e| AgentError::LlmResponseParse(e.to_string()))?;

        Ok(LlmResponse {
            stop_reason: api_response.stop_reason,
            content: api_response.content,
        })
    }
}
