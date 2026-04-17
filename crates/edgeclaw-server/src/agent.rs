use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_core::{
    Agent, AgentContext, ContentBlock, LlmClient, LlmConfig, Message, PermissionCheck, PolicyChain,
    ReqwestBackend, Role, ToolCall, ToolExecutor, ToolResult,
};
use serde_json::Value;
use skill_registry::{SkillRegistry, SkillRow};
use sqlx::SqlitePool;
use tokio::sync::{mpsc, oneshot};

use crate::builtin_executor::BuiltinExecutor;
use crate::server::ServerConfig;
use crate::session::ServerMessage;

/// How tool approvals are resolved during an agent turn.
pub enum ApprovalMode {
    /// All tools auto-approved (used by scheduler, HTTP fallback).
    AutoApprove,
    /// Approvals go through a session channel (used by WebSocket clients).
    Session {
        server_tx: mpsc::Sender<ServerMessage>,
        pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    },
}

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

async fn load_soul(pool: &SqlitePool, user_id: &str) -> agent_core::soul::Soul {
    let row = sqlx::query_as::<_, (String, String, String, String, String, String)>(
        "SELECT name, personality, archetype, tone, verbosity, decision_style FROM souls WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);

    match row {
        Some((name, personality, archetype, tone, verbosity, decision_style)) => {
            use std::str::FromStr;
            agent_core::soul::Soul {
                name,
                personality,
                archetype: agent_core::soul::Archetype::from_str(&archetype).unwrap_or_default(),
                tone: agent_core::soul::Tone::from_str(&tone).unwrap_or_default(),
                verbosity: agent_core::soul::Verbosity::from_str(&verbosity).unwrap_or_default(),
                decision_style: agent_core::soul::DecisionStyle::from_str(&decision_style)
                    .unwrap_or_default(),
            }
        }
        None => {
            // Backward compat: check prefs table for a custom system_prompt
            let pref = sqlx::query_scalar::<_, String>(
                "SELECT value FROM prefs WHERE user_id = ? AND key = 'system_prompt'",
            )
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

            match pref {
                Some(custom_prompt) => agent_core::soul::Soul {
                    name: String::new(),
                    personality: custom_prompt,
                    ..agent_core::soul::Soul::default()
                },
                None => agent_core::soul::Soul::default(),
            }
        }
    }
}

/// Build the full system prompt from soul + skill contexts.
async fn build_system_prompt(
    pool: &SqlitePool,
    user_id: &str,
    skill_rows: &[SkillRow],
    system_hint: Option<&str>,
) -> String {
    let soul = load_soul(pool, user_id).await;
    let mut prompt = agent_core::soul::compose_system_prompt(&soul);

    if let Some(hint) = system_hint {
        prompt.push_str("\n\n");
        prompt.push_str(hint);
    }

    for row in skill_rows {
        if let Some(ctx) = &row.skill_context {
            prompt.push_str("\n\n");
            prompt.push_str(ctx);
        }
    }

    prompt
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

/// Run a full agent turn: send message, check permissions, execute tools, persist results.
///
/// The harness drives the tool loop — the agent-core loop returns tool calls
/// without executing them, and the harness checks permissions via PolicyChain.
/// Safe tools are executed and resumed automatically. Tools needing approval
/// are persisted and returned to the client.
pub async fn run_agent_turn(
    pool: &SqlitePool,
    config: &ServerConfig,
    user_id: &str,
    user_message: &str,
    system_hint: Option<&str>,
    approval_mode: ApprovalMode,
) -> Result<Value, anyhow::Error> {
    ensure_user(pool, user_id).await?;

    let messages = load_messages(pool, user_id, 50).await;
    let skill_rows = load_skills(pool, user_id).await;
    let system_prompt = build_system_prompt(pool, user_id, &skill_rows, system_hint).await;

    let llm_config = build_llm_config(config);
    let registry = build_registry(skill_rows)?;

    let executor = BuiltinExecutor::new(
        pool.clone(),
        user_id.to_string(),
        registry,
        PolicyChain::default_chain(),
    );
    let tools = executor.all_tools();
    let llm = LlmClient::new(llm_config, ReqwestBackend::new());

    // Agent runs WITHOUT an executor — it returns tool calls to the harness
    let agent = Agent::new(llm);

    let ctx = AgentContext {
        system_prompt,
        messages,
        tools,
    };

    let mut result = agent.run(ctx, user_message).await?;

    // Harness-driven tool loop: check permissions, execute safe tools, resume
    loop {
        if result.pending_tool_calls.is_empty() {
            break;
        }

        // Process each tool call: check permission, get approval if needed, execute
        let mut tool_results = Vec::with_capacity(result.pending_tool_calls.len());
        for tc in &result.pending_tool_calls {
            let permission = executor.check_permission(tc);

            if !matches!(permission, PermissionCheck::Allow) {
                match &approval_mode {
                    ApprovalMode::AutoApprove => {
                        // Fall through to execute
                    }
                    ApprovalMode::Session {
                        server_tx,
                        pending_approvals,
                    } => {
                        let request_id = uuid::Uuid::new_v4().to_string();
                        let (resp_tx, resp_rx) = oneshot::channel();

                        // Register the oneshot for this approval request
                        pending_approvals
                            .lock()
                            .unwrap()
                            .insert(request_id.clone(), resp_tx);

                        let reason = match &permission {
                            PermissionCheck::RequiresApproval(r) | PermissionCheck::Deny(r) => {
                                r.clone()
                            }
                            _ => String::new(),
                        };

                        // Send confirmation prompt to client
                        let _ = server_tx
                            .send(ServerMessage::ConfirmationPrompt {
                                request_id,
                                tool_calls: vec![tc.clone()],
                                reasons: vec![reason],
                            })
                            .await;

                        // Block until client responds or timeout (5 min)
                        let approved = match tokio::time::timeout(
                            std::time::Duration::from_secs(300),
                            resp_rx,
                        )
                        .await
                        {
                            Ok(Ok(v)) => v,
                            _ => false, // Timeout or channel dropped -> auto-deny
                        };

                        if !approved {
                            tool_results.push(ToolResult::error_for(
                                tc.id.clone(),
                                "Permission to use tool was denied",
                            ));
                            continue;
                        }
                        // Approved — fall through to execute
                    }
                }
            }

            let tr = executor
                .execute(tc)
                .await
                .unwrap_or_else(|e| ToolResult::error_for(tc.id.clone(), e));

            // Send progress through session channel if present
            if let ApprovalMode::Session { server_tx, .. } = &approval_mode {
                let _ = server_tx
                    .send(ServerMessage::ToolExecuted {
                        tool_name: tc.name.clone(),
                        success: !tr.is_error,
                    })
                    .await;
            }

            tool_results.push(tr);
        }

        // Rebuild context from persisted + new messages.
        // Re-compose the soul prompt (soul may not change mid-turn, but this
        // keeps the prompt consistent with what was sent initially).
        let mut ctx_messages = load_messages(pool, user_id, 50).await;
        ctx_messages.extend(result.new_messages.clone());
        let soul = load_soul(pool, user_id).await;
        let mut system_prompt = agent_core::soul::compose_system_prompt(&soul);
        if let Some(hint) = system_hint {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(hint);
        }

        let ctx = AgentContext {
            system_prompt,
            messages: ctx_messages,
            tools: executor.all_tools(),
        };

        let resumed = agent.resume(ctx, tool_results).await?;
        result.new_messages.extend(resumed.new_messages);
        result.answer = resumed.answer;
        result.pending_tool_calls = resumed.pending_tool_calls;
    }

    persist_messages(pool, user_id, &result.new_messages).await;

    // Send final response through session channel if present
    if let ApprovalMode::Session { server_tx, .. } = &approval_mode {
        let _ = server_tx
            .send(ServerMessage::AgentResponse {
                answer: result.answer.clone(),
            })
            .await;
    }

    Ok(serde_json::json!({
        "answer": result.answer,
        "pending_tool_calls": result.pending_tool_calls,
    }))
}

/// Handle approval or denial of pending tool calls, then resume agent.
///
/// All pending tool calls for the user are resolved together since they
/// belong to the same assistant message. On approval, all are executed.
/// On denial, error ToolResults are returned so the LLM sees the denial.
pub async fn handle_approval(
    pool: &SqlitePool,
    config: &ServerConfig,
    user_id: &str,
    approval_id: i64,
    approve: bool,
) -> Result<Value, anyhow::Error> {
    // Verify the target approval exists
    let _row = sqlx::query_as::<_, (String,)>(
        "SELECT tool_call FROM pending_approvals WHERE id = ? AND user_id = ?",
    )
    .bind(approval_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Pending approval {} not found", approval_id))?;

    // Load ALL pending tool calls for this user — they all belong to the
    // same assistant message and must all get tool_result blocks.
    let all_rows = sqlx::query_as::<_, (String,)>(
        "SELECT tool_call FROM pending_approvals WHERE user_id = ? ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let all_tool_calls: Vec<ToolCall> = all_rows
        .iter()
        .map(|r| serde_json::from_str(&r.0).expect("valid tool call JSON"))
        .collect();

    // Clear all pending approvals
    sqlx::query("DELETE FROM pending_approvals WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;

    let messages = load_messages(pool, user_id, 50).await;
    let skill_rows = load_skills(pool, user_id).await;
    let system_prompt = build_system_prompt(pool, user_id, &skill_rows, None).await;
    let llm_config = build_llm_config(config);

    let registry = build_registry(skill_rows)?;
    let executor = BuiltinExecutor::new(
        pool.clone(),
        user_id.to_string(),
        registry,
        PolicyChain::default_chain(),
    );
    let tools = executor.all_tools();

    // Build tool results for ALL pending tool calls
    let tool_results: Vec<ToolResult> = if approve {
        let mut results = Vec::with_capacity(all_tool_calls.len());
        for tc in &all_tool_calls {
            let tr = executor
                .execute(tc)
                .await
                .unwrap_or_else(|e| ToolResult::error_for(tc.id.clone(), e));
            results.push(tr);
        }
        results
    } else {
        // Denied: return error ToolResults so the LLM sees the denial
        all_tool_calls
            .iter()
            .map(|tc| ToolResult::error_for(tc.id.clone(), "Permission to use tool was denied"))
            .collect()
    };

    let ctx = AgentContext {
        system_prompt,
        messages,
        tools,
    };

    let llm = LlmClient::new(llm_config, ReqwestBackend::new());
    let agent = Agent::new(llm);

    let agent_result = agent.resume(ctx, tool_results).await?;
    persist_messages(pool, user_id, &agent_result.new_messages).await;

    // If more tool calls surface, check permissions via the harness
    if !agent_result.pending_tool_calls.is_empty() {
        let needs_approval = agent_result
            .pending_tool_calls
            .iter()
            .any(|tc| !matches!(executor.check_permission(tc), PermissionCheck::Allow));

        if needs_approval {
            for tc in &agent_result.pending_tool_calls {
                persist_pending_approval(pool, user_id, tc).await;
            }
            return Ok(serde_json::json!({
                "status": "awaiting_approval",
                "answer": agent_result.answer,
                "pending_approvals": agent_result.pending_tool_calls,
            }));
        }
    }

    let status = if approve { "approved" } else { "denied" };
    Ok(serde_json::json!({
        "status": status,
        "answer": agent_result.answer,
        "pending_tool_calls": agent_result.pending_tool_calls,
    }))
}
