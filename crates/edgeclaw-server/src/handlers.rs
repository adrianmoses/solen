use agent_core::ReqwestBackend;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
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
