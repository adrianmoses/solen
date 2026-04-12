use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agent_core::ReqwestBackend;
use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use skill_registry::SkillRow;
use tokio::sync::mpsc;

use crate::agent;
use crate::server::AppState;
use crate::session::{ClientMessage, ServerMessage, SessionHandle};

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
    let result = agent::run_agent_turn(
        &state.db,
        &state.config,
        &body.user_id,
        &body.message,
        None,
        agent::ApprovalMode::AutoApprove,
    )
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

pub async fn clear_history_handler(
    State(state): State<AppState>,
    Query(params): Query<UserIdQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let del_messages = sqlx::query("DELETE FROM messages WHERE user_id = ?")
        .bind(&params.user_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    let del_approvals = sqlx::query("DELETE FROM pending_approvals WHERE user_id = ?")
        .bind(&params.user_id)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "status": "cleared",
        "messages_deleted": del_messages.rows_affected(),
        "approvals_deleted": del_approvals.rows_affected(),
    })))
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
            skill_context: None,
            session_id: None,
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
        "INSERT OR REPLACE INTO skills (user_id, name, url, tools, added_at, auth_header_name, auth_header_value, session_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&body.user_id)
    .bind(&row.name)
    .bind(&row.url)
    .bind(&row.tools_json)
    .bind(row.added_at)
    .bind(&row.auth_header_name)
    .bind(&row.auth_header_value)
    .bind(&row.session_id)
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

// --- Skill management handlers ---

pub async fn skill_status_handler(
    State(state): State<AppState>,
    Query(params): Query<UserIdQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT name, url, tools FROM skills WHERE user_id = ?",
    )
    .bind(&params.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_error)?;

    let mut statuses = Vec::new();
    for (name, url, tools_json) in rows {
        let tool_count = serde_json::from_str::<Vec<serde_json::Value>>(&tools_json)
            .map(|v| v.len())
            .unwrap_or(0);

        // Attempt MCP initialize to verify connectivity (5s timeout)
        let backend = ReqwestBackend::new();
        let client = mcp_client::McpClient::new(backend, url.clone(), vec![]);
        let status = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.initialize(),
        )
        .await
        {
            Ok(Ok(_)) => "connected".to_string(),
            Ok(Err(e)) => format!("error: {e}"),
            Err(_) => "error: connection timed out".to_string(),
        };

        statuses.push(serde_json::json!({
            "name": name,
            "url": url,
            "status": status,
            "tool_count": tool_count,
        }));
    }

    Ok(Json(serde_json::json!(statuses)))
}

pub async fn remove_skill_handler(
    State(state): State<AppState>,
    Path(skill_name): Path<String>,
    Query(params): Query<UserIdQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let result = sqlx::query("DELETE FROM skills WHERE user_id = ? AND name = ?")
        .bind(&params.user_id)
        .bind(&skill_name)
        .execute(&state.db)
        .await
        .map_err(internal_error)?;

    if result.rows_affected() == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "skill not found" })),
        ));
    }

    Ok(Json(
        serde_json::json!({ "removed": true, "name": skill_name }),
    ))
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

// --- WebSocket handler ---

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_connection(socket, state))
}

async fn handle_ws_connection(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();

    // Wait for handshake message with user_id
    let user_id = match stream.next().await {
        Some(Ok(WsMessage::Text(text))) => {
            let msg: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => return,
            };
            match msg.get("user_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => return,
            }
        }
        _ => return,
    };

    let session_id = uuid::Uuid::new_v4().to_string();

    // Create channels
    let (server_tx, mut server_rx) = mpsc::channel::<ServerMessage>(32);
    let pending_approvals: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Register session
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(
            session_id.clone(),
            SessionHandle {
                server_tx: server_tx.clone(),
                user_id: user_id.clone(),
                pending_approvals: pending_approvals.clone(),
            },
        );
    }

    // Send session_started to client
    let _ = sink
        .send(WsMessage::Text(
            serde_json::to_string(&ServerMessage::SessionStarted {
                session_id: session_id.clone(),
            })
            .unwrap()
            .into(),
        ))
        .await;

    // Outbound pump: server_rx -> WebSocket sink
    let outbound = tokio::spawn(async move {
        while let Some(msg) = server_rx.recv().await {
            let text = serde_json::to_string(&msg).unwrap();
            if sink.send(WsMessage::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // Inbound pump: WebSocket stream -> dispatch
    let state2 = state.clone();
    let user_id2 = user_id.clone();
    let inbound = tokio::spawn(async move {
        while let Some(Ok(ws_msg)) = stream.next().await {
            let text = match ws_msg {
                WsMessage::Text(t) => t.to_string(),
                WsMessage::Close(_) => break,
                _ => continue,
            };

            let client_msg: ClientMessage = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(_) => continue,
            };

            match client_msg {
                ClientMessage::UserMessage { message } => {
                    let db = state2.db.clone();
                    let config = state2.config.clone();
                    let uid = user_id2.clone();
                    let stx = server_tx.clone();
                    let pa = pending_approvals.clone();

                    tokio::spawn(async move {
                        let approval_mode = agent::ApprovalMode::Session {
                            server_tx: stx.clone(),
                            pending_approvals: pa,
                        };
                        if let Err(e) =
                            agent::run_agent_turn(&db, &config, &uid, &message, None, approval_mode)
                                .await
                        {
                            let _ = stx
                                .send(ServerMessage::AgentError {
                                    error: e.to_string(),
                                })
                                .await;
                        }
                    });
                }
                ClientMessage::ApprovalResponse {
                    request_id,
                    approved,
                } => {
                    let mut pending = pending_approvals.lock().unwrap();
                    if let Some(tx) = pending.remove(&request_id) {
                        let _ = tx.send(approved);
                    }
                }
            }
        }
    });

    // Wait for either task to finish (disconnect)
    tokio::select! {
        _ = outbound => {}
        _ = inbound => {}
    }

    // Cleanup session
    state.sessions.write().await.remove(&session_id);
}
