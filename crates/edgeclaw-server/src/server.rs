use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    routing::{delete, get, post},
    Json, Router,
};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::handlers;
use crate::oauth::{OAuthFlows, ProviderConfig};
use crate::session::SessionRegistry;

/// Configuration for an auto-registered MCP skill.
#[derive(Debug, Clone)]
pub struct SkillAutoConfig {
    pub name: String,
    pub url: String,
    /// Optional Bearer token for Authorization header (e.g. GitHub PAT).
    pub auth_token: Option<String>,
}

pub struct ServerConfig {
    pub database_url: String,
    pub host: String,
    pub port: u16,
    pub anthropic_api_key: Option<String>,
    pub default_model: Option<String>,
    pub anthropic_base_url: String,
    pub max_tasks_per_user: i64,
    pub token_master_key: Option<[u8; 32]>,
    pub providers: HashMap<String, ProviderConfig>,
    pub oauth_redirect_uri: String,
    pub skill_configs: Vec<SkillAutoConfig>,
    pub default_user_id: String,
}

impl ServerConfig {
    pub fn from_env() -> Self {
        Self {
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://edgeclaw.db?mode=rwc".to_string()),
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            port: std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080),
            anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            default_model: std::env::var("CLAUDE_MODEL").ok(),
            anthropic_base_url: std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".to_string()),
            max_tasks_per_user: std::env::var("MAX_TASKS_PER_USER")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20),
            token_master_key: match std::env::var("TOKEN_MASTER_KEY") {
                Err(_) => {
                    tracing::warn!(
                        "TOKEN_MASTER_KEY not set — credential storage will be unavailable"
                    );
                    None
                }
                Ok(b64) => {
                    use base64::Engine;
                    match base64::engine::general_purpose::STANDARD.decode(&b64) {
                        Ok(bytes) if bytes.len() == 32 => {
                            Some(<[u8; 32]>::try_from(bytes.as_slice()).unwrap())
                        }
                        _ => {
                            tracing::error!(
                                "TOKEN_MASTER_KEY is invalid (expected base64-encoded 32 bytes) \
                                 — credential storage will be unavailable"
                            );
                            None
                        }
                    }
                }
            },
            providers: Self::load_providers(),
            oauth_redirect_uri: std::env::var("OAUTH_REDIRECT_URI")
                .unwrap_or_else(|_| "http://localhost:8080/oauth/callback".to_string()),
            skill_configs: Self::load_skill_configs(),
            default_user_id: std::env::var("DEFAULT_USER_ID")
                .unwrap_or_else(|_| "default".to_string()),
        }
    }

    fn load_providers() -> HashMap<String, ProviderConfig> {
        let mut providers = HashMap::new();

        if let (Ok(id), Ok(secret)) = (
            std::env::var("GITHUB_CLIENT_ID"),
            std::env::var("GITHUB_CLIENT_SECRET"),
        ) {
            providers.insert(
                "github".to_string(),
                ProviderConfig {
                    client_id: id,
                    client_secret: secret,
                    auth_url: "https://github.com/login/oauth/authorize".to_string(),
                    token_url: "https://github.com/login/oauth/access_token".to_string(),
                    default_scopes: "repo,user:email".to_string(),
                    extra_auth_params: vec![],
                },
            );
        }

        if let (Ok(id), Ok(secret)) = (
            std::env::var("GOOGLE_CLIENT_ID"),
            std::env::var("GOOGLE_CLIENT_SECRET"),
        ) {
            providers.insert(
                "google".to_string(),
                ProviderConfig {
                    client_id: id,
                    client_secret: secret,
                    auth_url: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
                    token_url: "https://oauth2.googleapis.com/token".to_string(),
                    default_scopes: String::new(),
                    extra_auth_params: vec![
                        ("access_type".to_string(), "offline".to_string()),
                        ("prompt".to_string(), "consent".to_string()),
                    ],
                },
            );
        }

        providers
    }

    /// Parse SKILL_*_URL env vars into auto-config entries.
    /// e.g. SKILL_GOOGLE_WORKSPACE_URL=http://workspace-mcp:8000
    ///      → SkillAutoConfig { name: "google_workspace", url: "http://workspace-mcp:8000" }
    /// Also picks up SKILL_{NAME}_AUTH_TOKEN for Bearer auth (e.g. SKILL_GITHUB_AUTH_TOKEN).
    fn load_skill_configs() -> Vec<SkillAutoConfig> {
        // First pass: collect URLs
        let mut configs: Vec<SkillAutoConfig> = Vec::new();
        for (key, value) in std::env::vars() {
            if let Some(rest) = key.strip_prefix("SKILL_") {
                if let Some(name_upper) = rest.strip_suffix("_URL") {
                    let name = name_upper.to_lowercase();
                    configs.push(SkillAutoConfig {
                        name,
                        url: value,
                        auth_token: None,
                    });
                }
            }
        }
        // Second pass: attach auth tokens
        for config in &mut configs {
            let token_key = format!("SKILL_{}_AUTH_TOKEN", config.name.to_uppercase());
            if let Ok(token) = std::env::var(&token_key) {
                config.auth_token = Some(token);
            }
        }
        configs
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<ServerConfig>,
    pub oauth_flows: OAuthFlows,
    pub sessions: SessionRegistry,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/message", post(handlers::message_handler))
        .route(
            "/history",
            get(handlers::history_handler).delete(handlers::clear_history_handler),
        )
        .route("/skills/add", post(handlers::add_skill_handler))
        .route("/skills", get(handlers::list_skills_handler))
        .route("/approve", post(handlers::approve_handler))
        .route("/approvals", get(handlers::list_approvals_handler))
        .route("/tasks/schedule", post(handlers::schedule_task_handler))
        .route("/tasks", get(handlers::list_tasks_handler))
        .route("/tasks/{id}", delete(handlers::delete_task_handler))
        .route("/oauth/start", post(handlers::oauth_start_handler))
        .route("/oauth/callback", get(handlers::oauth_callback_handler))
        .route(
            "/credentials/import-service-account",
            post(handlers::import_service_account_handler),
        )
        .route("/ws", get(handlers::ws_handler))
        .route("/admin/skills/status", get(handlers::skill_status_handler))
        .route("/skills/{name}", delete(handlers::remove_skill_handler))
        .with_state(state)
}
