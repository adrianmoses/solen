use std::sync::Arc;

use axum::{
    routing::{delete, get, post},
    Json, Router,
};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::handlers;

pub struct ServerConfig {
    pub database_url: String,
    pub host: String,
    pub port: u16,
    pub anthropic_api_key: Option<String>,
    pub default_model: Option<String>,
    pub anthropic_base_url: String,
    pub max_tasks_per_user: i64,
    pub token_master_key: Option<[u8; 32]>,
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
        }
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<ServerConfig>,
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
        .route("/history", get(handlers::history_handler))
        .route("/skills/add", post(handlers::add_skill_handler))
        .route("/skills", get(handlers::list_skills_handler))
        .route("/approve", post(handlers::approve_handler))
        .route("/approvals", get(handlers::list_approvals_handler))
        .route("/tasks/schedule", post(handlers::schedule_task_handler))
        .route("/tasks", get(handlers::list_tasks_handler))
        .route("/tasks/{id}", delete(handlers::delete_task_handler))
        .with_state(state)
}
