use std::str::FromStr;

use agent_core::ReqwestBackend;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Html,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use skill_registry::SkillRow;

use crate::agent;
use crate::server::AppState;

// --- Request/Response types ---

#[derive(Deserialize)]
pub struct MessageRequest {
    pub user_id: String,
    pub message: String,
}

#[derive(Deserialize)]
pub struct UserIdQuery {
    pub user_id: String,
}

#[derive(Deserialize)]
pub struct AddSkillRequest {
    pub user_id: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub auth_header_name: Option<String>,
    #[serde(default)]
    pub auth_header_value: Option<String>,
}

#[derive(Deserialize)]
pub struct ApproveRequest {
    pub user_id: String,
    pub id: i64,
    #[serde(default)]
    pub approve: bool,
}

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": e.to_string() })),
    )
}

// --- Handlers ---

pub async fn message_handler(
    State(state): State<AppState>,
    Json(body): Json<MessageRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let result =
        agent::run_agent_turn(&state.db, &state.config, &body.user_id, &body.message, None)
            .await
            .map_err(internal_error)?;
    Ok(Json(result))
}

pub async fn history_handler(
    State(state): State<AppState>,
    Query(params): Query<UserIdQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let rows = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT role, content, created_at FROM messages WHERE user_id = ? ORDER BY id ASC LIMIT 50",
    )
    .bind(&params.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    let messages: Vec<serde_json::Value> = rows
        .into_iter()
        .filter_map(|(role, content, created_at)| {
            let content: serde_json::Value = serde_json::from_str(&content).ok()?;
            Some(serde_json::json!({
                "role": role,
                "content": content,
                "created_at": created_at,
            }))
        })
        .collect();

    Ok(Json(serde_json::json!(messages)))
}

pub async fn add_skill_handler(
    State(state): State<AppState>,
    Json(body): Json<AddSkillRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let now = chrono::Utc::now().timestamp_millis();
    agent::ensure_user(&state.db, &body.user_id)
        .await
        .map_err(internal_error)?;

    // Load existing skills to build registry
    let existing_rows = sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT name, url, tools, added_at FROM skills WHERE user_id = ?",
    )
    .bind(&body.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    let skill_rows: Vec<SkillRow> = existing_rows
        .into_iter()
        .map(|(name, url, tools_json, added_at)| SkillRow {
            name,
            url,
            tools_json,
            added_at,
            auth_header_name: None,
            auth_header_value: None,
        })
        .collect();

    let mut registry =
        skill_registry::SkillRegistry::<ReqwestBackend>::from_rows(skill_rows, ReqwestBackend::new)
            .map_err(internal_error)?;

    let row = registry
        .register(
            body.name.clone(),
            body.url.clone(),
            ReqwestBackend::new(),
            now,
            body.auth_header_name.clone(),
            body.auth_header_value.clone(),
        )
        .await
        .map_err(internal_error)?;

    let tool_names: Vec<String> = registry
        .all_tools()
        .iter()
        .filter(|t| t.name.starts_with(&row.name))
        .map(|t| t.name.clone())
        .collect();

    // Persist skill
    sqlx::query(
        "INSERT OR REPLACE INTO skills (user_id, name, url, tools, added_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&body.user_id)
    .bind(&row.name)
    .bind(&row.url)
    .bind(&row.tools_json)
    .bind(row.added_at)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "skill": row.name,
        "tools": tool_names,
    })))
}

pub async fn list_skills_handler(
    State(state): State<AppState>,
    Query(params): Query<UserIdQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let rows = sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT name, url, tools, added_at FROM skills WHERE user_id = ?",
    )
    .bind(&params.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    let skills: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(name, url, tools_json, added_at)| {
            serde_json::json!({
                "name": name,
                "url": url,
                "tools_json": tools_json,
                "added_at": added_at,
            })
        })
        .collect();

    Ok(Json(serde_json::json!(skills)))
}

pub async fn approve_handler(
    State(state): State<AppState>,
    Json(body): Json<ApproveRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let result = agent::handle_approval(
        &state.db,
        &state.config,
        &body.user_id,
        body.id,
        body.approve,
    )
    .await
    .map_err(internal_error)?;
    Ok(Json(result))
}

pub async fn list_approvals_handler(
    State(state): State<AppState>,
    Query(params): Query<UserIdQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let rows = sqlx::query_as::<_, (i64, String, i64)>(
        "SELECT id, tool_call, created_at FROM pending_approvals WHERE user_id = ?",
    )
    .bind(&params.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    let approvals: Vec<serde_json::Value> = rows
        .into_iter()
        .filter_map(|(id, tool_call_json, created_at)| {
            let tool_call: serde_json::Value = serde_json::from_str(&tool_call_json).ok()?;
            Some(serde_json::json!({
                "id": id,
                "tool_call": tool_call,
                "created_at": created_at,
            }))
        })
        .collect();

    Ok(Json(serde_json::json!(approvals)))
}

// --- Scheduled task handlers ---

#[derive(Deserialize)]
pub struct ScheduleTaskRequest {
    pub user_id: String,
    pub name: String,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub run_at: Option<i64>,
    pub payload: serde_json::Value,
}

fn bad_request(msg: impl std::fmt::Display) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg.to_string() })),
    )
}

pub async fn schedule_task_handler(
    State(state): State<AppState>,
    Json(body): Json<ScheduleTaskRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Validate: exactly one of cron or run_at
    let (cron_expr, run_at) = match (&body.cron, body.run_at) {
        (Some(expr), None) => {
            // Validate cron expression and compute initial run_at
            let schedule = cron::Schedule::from_str(expr)
                .map_err(|e| bad_request(format!("invalid cron expression: {e}")))?;
            let next = schedule
                .upcoming(Utc)
                .next()
                .ok_or_else(|| bad_request("cron expression has no upcoming runs"))?;
            (Some(expr.clone()), next.timestamp_millis())
        }
        (None, Some(ts)) => (None, ts),
        _ => {
            return Err(bad_request(
                "exactly one of 'cron' or 'run_at' must be provided",
            ))
        }
    };

    agent::ensure_user(&state.db, &body.user_id)
        .await
        .map_err(internal_error)?;

    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM scheduled_tasks WHERE user_id = ? AND enabled = 1")
            .bind(&body.user_id)
            .fetch_one(&state.db)
            .await
            .map_err(internal_error)?;

    if count >= state.config.max_tasks_per_user {
        return Err(bad_request(format!(
            "maximum of {} active tasks per user",
            state.config.max_tasks_per_user
        )));
    }

    let payload_str = serde_json::to_string(&body.payload).map_err(internal_error)?;

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO scheduled_tasks (user_id, name, cron, run_at, payload) VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&body.user_id)
    .bind(&body.name)
    .bind(&cron_expr)
    .bind(run_at)
    .bind(&payload_str)
    .fetch_one(&state.db)
    .await
    .map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "id": id,
        "name": body.name,
        "next_run_at": run_at,
    })))
}

pub async fn list_tasks_handler(
    State(state): State<AppState>,
    Query(params): Query<UserIdQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let rows = sqlx::query_as::<_, (i64, String, Option<String>, Option<i64>, String, Option<i64>)>(
        "SELECT id, name, cron, run_at, payload, last_run FROM scheduled_tasks WHERE user_id = ? AND enabled = 1",
    )
    .bind(&params.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    let tasks: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, name, cron, run_at, payload, last_run)| {
            serde_json::json!({
                "id": id,
                "name": name,
                "cron": cron,
                "run_at": run_at,
                "payload": serde_json::from_str::<serde_json::Value>(&payload).unwrap_or_default(),
                "last_run": last_run,
            })
        })
        .collect();

    Ok(Json(serde_json::json!(tasks)))
}

pub async fn delete_task_handler(
    State(state): State<AppState>,
    Path(task_id): Path<i64>,
    Query(params): Query<UserIdQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let result = sqlx::query(
        "UPDATE scheduled_tasks SET enabled = 0 WHERE id = ? AND user_id = ? AND enabled = 1",
    )
    .bind(task_id)
    .bind(&params.user_id)
    .execute(&state.db)
    .await
    .map_err(internal_error)?;

    if result.rows_affected() == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "task not found" })),
        ));
    }

    Ok(Json(serde_json::json!({ "deleted": true, "id": task_id })))
}

// --- Service account import handler ---

#[derive(Deserialize)]
pub struct ImportServiceAccountRequest {
    pub user_id: String,
    pub skill_name: String,
    pub provider: String,
    pub scopes: String,
    pub service_account_json: serde_json::Value,
}

pub async fn import_service_account_handler(
    State(state): State<AppState>,
    Json(body): Json<ImportServiceAccountRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let master_key = state
        .config
        .token_master_key
        .as_ref()
        .ok_or_else(|| bad_request("master key not configured"))?;

    let private_key = body
        .service_account_json
        .get("private_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad_request("service_account_json missing 'private_key'"))?;

    let client_email = body
        .service_account_json
        .get("client_email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad_request("service_account_json missing 'client_email'"))?;

    let token_uri = body
        .service_account_json
        .get("token_uri")
        .and_then(|v| v.as_str())
        .unwrap_or("https://oauth2.googleapis.com/token");

    let metadata = credential_store::ServiceAccountMetadata {
        client_email: client_email.to_string(),
        token_uri: token_uri.to_string(),
    };

    credential_store::CredentialStore::store_service_account(
        &state.db,
        master_key,
        &body.user_id,
        &body.skill_name,
        &body.provider,
        private_key,
        &metadata,
        &body.scopes,
    )
    .await
    .map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "credential_type": "service_account",
        "provider": body.provider,
        "skill_name": body.skill_name,
    })))
}

// --- OAuth handlers ---

#[derive(Deserialize)]
pub struct OAuthStartRequest {
    pub user_id: String,
    pub skill_name: String,
    pub provider: String,
    pub scopes: Option<String>,
}

#[derive(Deserialize)]
pub struct OAuthCallbackQuery {
    pub code: String,
    pub state: String,
}

pub async fn oauth_start_handler(
    State(state): State<AppState>,
    Json(body): Json<OAuthStartRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let provider_config = state
        .config
        .providers
        .get(&body.provider)
        .ok_or_else(|| bad_request(format!("provider '{}' is not configured", body.provider)))?
        .clone();

    let (nonce, authorization_url) = crate::oauth::init_flow(
        &state.oauth_flows,
        body.user_id,
        body.skill_name,
        &provider_config,
        body.provider,
        &state.config.oauth_redirect_uri,
        body.scopes.as_deref(),
    );

    let _ = nonce; // nonce is embedded in the authorization_url as `state`

    Ok(Json(serde_json::json!({
        "authorization_url": authorization_url,
        "expires_in_seconds": 600,
    })))
}

pub async fn oauth_callback_handler(
    State(state): State<AppState>,
    Query(params): Query<OAuthCallbackQuery>,
) -> Html<String> {
    match handle_oauth_callback(&state, &params.code, &params.state).await {
        Ok(provider) => {
            let provider = html_escape(&provider);
            Html(format!(
                "<html><body><h1>Authorization successful</h1>\
                 <p>Your {provider} account has been connected. You can close this window.</p>\
                 </body></html>"
            ))
        }
        Err(e) => {
            let error = html_escape(&e.to_string());
            Html(format!(
                "<html><body><h1>Authorization failed</h1>\
                 <p>Error: {error}</p>\
                 </body></html>"
            ))
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

async fn handle_oauth_callback(
    state: &AppState,
    code: &str,
    nonce: &str,
) -> Result<String, crate::oauth::OAuthError> {
    let flow_state = crate::oauth::complete_flow(&state.oauth_flows, nonce)?;

    let provider_config = state
        .config
        .providers
        .get(&flow_state.provider)
        .ok_or_else(|| {
            crate::oauth::OAuthError::ProviderNotConfigured(flow_state.provider.clone())
        })?;

    let client = reqwest::Client::new();
    let token_resp = crate::oauth::exchange_code(
        &client,
        provider_config,
        code,
        &flow_state.code_verifier,
        &state.config.oauth_redirect_uri,
    )
    .await?;

    let master_key = state
        .config
        .token_master_key
        .as_ref()
        .ok_or(crate::oauth::OAuthError::MasterKeyNotConfigured)?;

    let expires_at = token_resp.expires_in.map(|ei| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before UNIX epoch")
            .as_secs() as i64
            + ei
    });

    credential_store::CredentialStore::store(
        &state.db,
        master_key,
        &flow_state.user_id,
        &flow_state.skill_name,
        &flow_state.provider,
        &token_resp.access_token,
        token_resp.refresh_token.as_deref(),
        expires_at,
        &flow_state.scopes,
    )
    .await
    .map_err(crate::oauth::OAuthError::CredentialStore)?;

    Ok(flow_state.provider)
}
