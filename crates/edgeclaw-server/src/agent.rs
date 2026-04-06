use std::sync::Arc;

use agent_core::{
    Agent, AgentContext, ContentBlock, LlmClient, LlmConfig, Message, PolicyChain, ReqwestBackend,
    Role, ToolCall, ToolExecutor, ToolResult,
};
use serde_json::Value;
use skill_registry::{SkillRegistry, SkillRow};
use sqlx::SqlitePool;

use crate::builtin_executor::BuiltinExecutor;
use crate::server::ServerConfig;

pub fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn build_llm_config(config: &ServerConfig) -> LlmConfig {
    LlmConfig {
        api_key: config.anthropic_api_key.clone().unwrap_or_default(),
        model: config
            .default_model
            .clone()
            .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string()),
        base_url: config.anthropic_base_url.clone(),
        max_tokens: 4096,
    }
}

pub async fn ensure_user(pool: &SqlitePool, user_id: &str) -> Result<(), sqlx::Error> {
    let now = now_millis();
    sqlx::query("INSERT OR IGNORE INTO users (id, created_at) VALUES (?, ?)")
        .bind(user_id)
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}

async fn load_messages(pool: &SqlitePool, user_id: &str, limit: i64) -> Vec<Message> {
    let rows = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT role, content, created_at FROM messages WHERE user_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut messages: Vec<Message> = rows
        .into_iter()
        .filter_map(|(role_str, content_json, created_at)| {
            let role = match role_str.as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => return None,
            };
            let content: Vec<ContentBlock> = serde_json::from_str(&content_json).ok()?;
            Some(Message {
                role,
                content,
                created_at,
            })
        })
        .collect();

    messages.reverse();
    messages
}

async fn persist_messages(pool: &SqlitePool, user_id: &str, messages: &[Message]) {
    let now = now_millis();
    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let content_json = serde_json::to_string(&msg.content).unwrap_or_default();
        let _ = sqlx::query(
            "INSERT INTO messages (user_id, role, content, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(role)
        .bind(&content_json)
        .bind(now)
        .execute(pool)
        .await;
    }
}

async fn load_system_prompt(pool: &SqlitePool, user_id: &str) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM prefs WHERE user_id = ? AND key = 'system_prompt'",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "You are a helpful AI assistant.".to_string())
}

async fn load_skills(pool: &SqlitePool, user_id: &str) -> Vec<SkillRow> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT name, url, tools, added_at, skill_context, auth_header_name, auth_header_value, session_id FROM skills WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(
            |(
                name,
                url,
                tools_json,
                added_at,
                skill_context,
                auth_header_name,
                auth_header_value,
                session_id,
            )| {
                SkillRow {
                    name,
                    url,
                    tools_json,
                    added_at,
                    auth_header_name,
                    auth_header_value,
                    skill_context,
                    session_id,
                }
            },
        )
        .collect()
}

fn build_registry(
    skill_rows: Vec<SkillRow>,
) -> Result<SkillRegistry<ReqwestBackend>, agent_core::AgentError> {
    SkillRegistry::from_rows(skill_rows, ReqwestBackend::new)
}

async fn persist_pending_approval(pool: &SqlitePool, user_id: &str, tool_call: &ToolCall) {
    let now = now_millis();
    let tc_json = serde_json::to_string(tool_call).unwrap_or_default();
    let _ = sqlx::query(
        "INSERT INTO pending_approvals (user_id, tool_call, created_at) VALUES (?, ?, ?)",
    )
    .bind(user_id)
    .bind(&tc_json)
    .bind(now)
    .execute(pool)
    .await;
}

/// Run a full agent turn: send message, execute tool loop, persist results.
pub async fn run_agent_turn(
    pool: &SqlitePool,
    config: &ServerConfig,
    user_id: &str,
    user_message: &str,
    system_hint: Option<&str>,
) -> Result<Value, anyhow::Error> {
    ensure_user(pool, user_id).await?;

    let messages = load_messages(pool, user_id, 50).await;
    let mut system_prompt = load_system_prompt(pool, user_id).await;
    if let Some(hint) = system_hint {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(hint);
    }
    let skill_rows = load_skills(pool, user_id).await;

    // Inject SKILL.md context into system prompt so the agent knows how to use skills
    for row in &skill_rows {
        if let Some(ctx) = &row.skill_context {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(ctx);
        }
    }

    let llm_config = build_llm_config(config);
    let registry = build_registry(skill_rows)?;

    // BuiltinExecutor composes built-in tools + MCP skills + permission policy
    let executor = BuiltinExecutor::new(
        pool.clone(),
        user_id.to_string(),
        registry,
        PolicyChain::default_chain(),
    );
    let tools = executor.all_tools();
    let executor: Arc<dyn ToolExecutor> = Arc::new(executor);
    let llm = LlmClient::new(llm_config, ReqwestBackend::new());
    let agent = Agent::new(llm).with_tool_executor(executor.clone());

    let ctx = AgentContext {
        system_prompt,
        messages,
        tools,
    };

    let result = agent.run(ctx, user_message).await?;
    persist_messages(pool, user_id, &result.new_messages).await;

    // The agent loop now handles safe tools inline. Only destructive tools
    // (where SkillRegistry::needs_approval returns true) appear here.
    if !result.pending_tool_calls.is_empty() {
        for tc in &result.pending_tool_calls {
            persist_pending_approval(pool, user_id, tc).await;
        }
        return Ok(serde_json::json!({
            "status": "awaiting_approval",
            "answer": result.answer,
            "pending_approvals": result.pending_tool_calls,
        }));
    }

    Ok(serde_json::json!({
        "answer": result.answer,
        "pending_tool_calls": result.pending_tool_calls,
    }))
}

/// Handle approval of a pending tool call, then resume agent.
pub async fn handle_approval(
    pool: &SqlitePool,
    config: &ServerConfig,
    user_id: &str,
    approval_id: i64,
    approve: bool,
) -> Result<Value, anyhow::Error> {
    let row = sqlx::query_as::<_, (String,)>(
        "SELECT tool_call FROM pending_approvals WHERE id = ? AND user_id = ?",
    )
    .bind(approval_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Pending approval {} not found", approval_id))?;

    let tool_call: ToolCall = serde_json::from_str(&row.0)?;

    sqlx::query("DELETE FROM pending_approvals WHERE id = ?")
        .bind(approval_id)
        .execute(pool)
        .await?;

    if !approve {
        return Ok(serde_json::json!({
            "status": "denied",
            "tool_call": tool_call,
        }));
    }

    let messages = load_messages(pool, user_id, 50).await;
    let system_prompt = load_system_prompt(pool, user_id).await;
    let skill_rows = load_skills(pool, user_id).await;
    let llm_config = build_llm_config(config);

    let registry = build_registry(skill_rows)?;
    let executor = BuiltinExecutor::new(
        pool.clone(),
        user_id.to_string(),
        registry,
        PolicyChain::default_chain(),
    );
    let tools = executor.all_tools();
    let executor: Arc<dyn ToolExecutor> = Arc::new(executor);

    let tool_result = executor
        .execute(&tool_call)
        .await
        .unwrap_or_else(|e| ToolResult::error_for(tool_call.id.clone(), e));

    let ctx = AgentContext {
        system_prompt,
        messages,
        tools,
    };

    let llm = LlmClient::new(llm_config, ReqwestBackend::new());
    let agent = Agent::new(llm).with_tool_executor(executor);

    let agent_result = agent.resume(ctx, vec![tool_result]).await?;
    persist_messages(pool, user_id, &agent_result.new_messages).await;

    // If more destructive tools surface, persist them
    if !agent_result.pending_tool_calls.is_empty() {
        for tc in &agent_result.pending_tool_calls {
            persist_pending_approval(pool, user_id, tc).await;
        }
        return Ok(serde_json::json!({
            "status": "awaiting_approval",
            "answer": agent_result.answer,
            "pending_approvals": agent_result.pending_tool_calls,
        }));
    }

    Ok(serde_json::json!({
        "status": "approved",
        "answer": agent_result.answer,
        "pending_tool_calls": agent_result.pending_tool_calls,
    }))
}
