use agent_core::{
    Agent, AgentContext, ContentBlock, LlmClient, LlmConfig, Message, ReqwestBackend, Role,
    ToolCall, ToolExecutor, ToolResult,
};
use serde_json::Value;
use skill_registry::{SkillRegistry, SkillRow};
use sqlx::SqlitePool;

use crate::server::ServerConfig;

// --- Destructive tool detection (ported from worker) ---

const DESTRUCTIVE_PATTERNS: &[&str] = &["delete", "remove", "send", "drop"];

fn is_destructive(tool_name: &str) -> bool {
    let lower = tool_name.to_lowercase();
    DESTRUCTIVE_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
}

fn check_destructive(tool_calls: &[ToolCall]) -> (Vec<ToolCall>, Vec<ToolCall>) {
    let mut safe = Vec::new();
    let mut destructive = Vec::new();
    for tc in tool_calls {
        if is_destructive(&tc.name) {
            destructive.push(tc.clone());
        } else {
            safe.push(tc.clone());
        }
    }
    (safe, destructive)
}

fn now_millis() -> i64 {
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
    let rows = sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT name, url, tools, added_at FROM skills WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|(name, url, tools_json, added_at)| SkillRow {
            name,
            url,
            tools_json,
            added_at,
            auth_header_name: None,
            auth_header_value: None,
        })
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

    let llm_config = build_llm_config(config);
    let registry = build_registry(skill_rows)?;
    let tools = registry.all_tools();

    let llm = LlmClient::new(llm_config, ReqwestBackend::new());
    let agent = Agent::new(llm);

    let ctx = AgentContext {
        system_prompt,
        messages,
        tools,
    };

    let mut result = agent.run(ctx, user_message).await?;
    persist_messages(pool, user_id, &result.new_messages).await;

    // Tool execution loop
    while !result.pending_tool_calls.is_empty() {
        let (safe_calls, destructive_calls) = check_destructive(&result.pending_tool_calls);

        // Persist destructive calls as pending approvals
        if !destructive_calls.is_empty() {
            for tc in &destructive_calls {
                persist_pending_approval(pool, user_id, tc).await;
            }

            if safe_calls.is_empty() {
                return Ok(serde_json::json!({
                    "status": "awaiting_approval",
                    "answer": result.answer,
                    "pending_approvals": destructive_calls,
                }));
            }
        }

        // Execute safe tool calls
        let mut tool_results = Vec::new();
        for tc in &safe_calls {
            let tr = registry.execute(tc).await.unwrap_or_else(|e| ToolResult {
                tool_use_id: tc.id.clone(),
                content: format!("Tool execution error: {e}"),
                is_error: true,
            });
            tool_results.push(tr);
        }

        // For destructive calls that were deferred
        for tc in &destructive_calls {
            tool_results.push(ToolResult {
                tool_use_id: tc.id.clone(),
                content: "This tool call requires human approval and is pending.".to_string(),
                is_error: true,
            });
        }

        // Reload context and resume
        let messages = load_messages(pool, user_id, 50).await;
        let system_prompt = load_system_prompt(pool, user_id).await;
        let tools = registry.all_tools();
        let llm_config = build_llm_config(config);

        let llm = LlmClient::new(llm_config, ReqwestBackend::new());
        let agent = Agent::new(llm);

        let ctx = AgentContext {
            system_prompt,
            messages,
            tools,
        };

        result = agent.resume(ctx, tool_results).await?;
        persist_messages(pool, user_id, &result.new_messages).await;
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
    let tools = registry.all_tools();

    let tool_result = registry
        .execute(&tool_call)
        .await
        .unwrap_or_else(|e| ToolResult {
            tool_use_id: tool_call.id.clone(),
            content: format!("Tool execution error: {e}"),
            is_error: true,
        });

    let ctx = AgentContext {
        system_prompt,
        messages,
        tools,
    };

    let llm = LlmClient::new(llm_config, ReqwestBackend::new());
    let agent = Agent::new(llm);

    let agent_result = agent.resume(ctx, vec![tool_result]).await?;
    persist_messages(pool, user_id, &agent_result.new_messages).await;

    Ok(serde_json::json!({
        "status": "approved",
        "answer": agent_result.answer,
        "pending_tool_calls": agent_result.pending_tool_calls,
    }))
}
