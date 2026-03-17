use std::cell::Cell;

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use agent_core::{
    Agent, AgentContext, AgentRunResult, ContentBlock, HttpBackend, LlmClient, LlmConfig, Message,
    Role, ToolCall, ToolExecutor, ToolResult,
};
use serde::Deserialize;
use skill_registry::{SkillRegistry, SkillRow};
use worker::*;

// --- HttpBackend implementation for worker::Fetch ---

struct WorkerFetchBackend;

#[async_trait::async_trait(?Send)]
impl HttpBackend for WorkerFetchBackend {
    async fn post(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<u8>, agent_core::AgentError> {
        let mut init = RequestInit::new();
        init.method = Method::Post;

        let body_str = String::from_utf8(body.to_vec())
            .map_err(|e| agent_core::AgentError::Http(e.to_string()))?;
        init.body = Some(wasm_bindgen::JsValue::from_str(&body_str));

        for (key, value) in headers {
            init.headers
                .set(key, value)
                .map_err(|e| agent_core::AgentError::Http(format!("{e:?}")))?;
        }

        let request = Request::new_with_init(url, &init)
            .map_err(|e| agent_core::AgentError::Http(format!("{e:?}")))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| agent_core::AgentError::Http(format!("{e:?}")))?;

        let bytes = response
            .bytes()
            .await
            .map_err(|e| agent_core::AgentError::Http(format!("{e:?}")))?;

        Ok(bytes)
    }
}

// --- Dispatcher Worker ---

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let path = req.path();

    // POST /orchestrate — multi-agent fan-out (M1.7)
    if req.method() == Method::Post && path.as_str() == "/orchestrate" {
        return handle_orchestrate(req, &env).await;
    }

    // Extract user ID from X-User-Id header or query param for local testing
    let user_id = req.headers().get("X-User-Id").ok().flatten().or_else(|| {
        url.query_pairs()
            .find(|(k, _)| k == "user_id")
            .map(|(_, v)| v.to_string())
    });

    let user_id = match user_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            return Response::error(
                "Missing user identity (X-User-Id header or user_id query param)",
                400,
            )
        }
    };

    // Get the AgentDO namespace and derive a deterministic ID
    let namespace = env.durable_object("AGENT_DO")?;
    let stub = namespace
        .id_from_name(&format!("agent:{user_id}"))?
        .get_stub()?;

    // Forward the request to the DO
    stub.fetch_with_request(req).await
}

// --- Multi-Agent Orchestration (M1.7) ---

#[derive(Deserialize)]
struct OrchestrateRequest {
    task: String,
    agents: Vec<String>,
}

async fn handle_orchestrate(mut req: Request, env: &Env) -> Result<Response> {
    let body: OrchestrateRequest = req.json().await?;
    let namespace = env.durable_object("AGENT_DO")?;

    let mut results = serde_json::Map::new();

    // Sequential fan-out: send the task to each named agent
    for agent_name in &body.agents {
        let stub = namespace
            .id_from_name(&format!("agent:{agent_name}"))?
            .get_stub()?;

        let message_body = serde_json::json!({ "message": body.task });

        let mut init = RequestInit::new();
        init.method = Method::Post;
        init.body = Some(wasm_bindgen::JsValue::from_str(
            &serde_json::to_string(&message_body).map_err(|e| Error::RustError(e.to_string()))?,
        ));
        init.headers
            .set("content-type", "application/json")
            .map_err(|e| Error::RustError(format!("{e:?}")))?;

        let inner_req = Request::new_with_init("https://fake-host/message", &init)?;
        let mut resp = stub.fetch_with_request(inner_req).await?;
        let value: serde_json::Value = resp.json().await?;
        results.insert(agent_name.clone(), value);
    }

    Response::from_json(&results)
}

// --- Destructive tool detection (M2.8) ---

const DESTRUCTIVE_PATTERNS: &[&str] = &["delete", "remove", "send", "drop"];

fn mask_secret(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 7 {
        "***".to_string()
    } else {
        let prefix: String = chars[..4].iter().collect();
        let suffix: String = chars[chars.len() - 3..].iter().collect();
        format!("{prefix}...{suffix}")
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

fn derive_encryption_key(secret: &str) -> Key<Aes256Gcm> {
    let mut key_bytes = [0u8; 32];
    let secret_bytes = secret.as_bytes();
    for (i, byte) in key_bytes.iter_mut().enumerate() {
        *byte = secret_bytes[i % secret_bytes.len()];
    }
    *Key::<Aes256Gcm>::from_slice(&key_bytes)
}

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

// --- AgentDO Durable Object ---

#[durable_object]
pub struct AgentDo {
    state: State,
    env: Env,
    initialized: Cell<bool>,
}

impl AgentDo {
    fn ensure_schema(&self) {
        if self.initialized.get() {
            return;
        }
        let sql = self.state.storage().sql();
        let none: Option<Vec<SqlStorageValue>> = None;
        let _ = sql.exec(
            "CREATE TABLE IF NOT EXISTS messages (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                role       TEXT    NOT NULL,
                content    TEXT    NOT NULL,
                created_at INTEGER NOT NULL
            )",
            none.clone(),
        );
        let _ = sql.exec(
            "CREATE TABLE IF NOT EXISTS skills (
                name       TEXT PRIMARY KEY,
                url        TEXT NOT NULL,
                tools      TEXT NOT NULL,
                added_at   INTEGER NOT NULL
            )",
            none.clone(),
        );
        let _ = sql.exec(
            "CREATE TABLE IF NOT EXISTS pending_approvals (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                tool_call  TEXT    NOT NULL,
                created_at INTEGER NOT NULL
            )",
            none.clone(),
        );
        let _ = sql.exec(
            "CREATE TABLE IF NOT EXISTS prefs (
                key        TEXT PRIMARY KEY,
                value      TEXT NOT NULL
            )",
            none.clone(),
        );
        // Migration: add auth columns to skills table (safe if already exist)
        let _ = sql.exec(
            "ALTER TABLE skills ADD COLUMN auth_header_name TEXT",
            none.clone(),
        );
        let _ = sql.exec("ALTER TABLE skills ADD COLUMN auth_header_value TEXT", none);
        self.initialized.set(true);
    }

    fn encrypt_secret(&self, plaintext: &str) -> String {
        let secret = match self.env.secret("SKILL_ENCRYPTION_KEY") {
            Ok(s) => s.to_string(),
            Err(_) => return plaintext.to_string(),
        };
        let key = derive_encryption_key(&secret);
        let cipher = Aes256Gcm::new(&key);

        let mut nonce_bytes = [0u8; 12];
        for byte in &mut nonce_bytes {
            *byte = (js_sys::Math::random() * 256.0) as u8;
        }
        let nonce = Nonce::from_slice(&nonce_bytes);

        match cipher.encrypt(nonce, plaintext.as_bytes()) {
            Ok(ciphertext) => {
                let mut combined = nonce_bytes.to_vec();
                combined.extend_from_slice(&ciphertext);
                format!("enc:{}", to_hex(&combined))
            }
            Err(_) => plaintext.to_string(),
        }
    }

    fn decrypt_secret(&self, stored: &str) -> String {
        let encrypted_hex = match stored.strip_prefix("enc:") {
            Some(hex) => hex,
            None => return stored.to_string(), // plaintext (legacy)
        };
        let secret = match self.env.secret("SKILL_ENCRYPTION_KEY") {
            Ok(s) => s.to_string(),
            Err(_) => return stored.to_string(),
        };
        let key = derive_encryption_key(&secret);
        let cipher = Aes256Gcm::new(&key);

        let bytes = match from_hex(encrypted_hex) {
            Some(b) if b.len() > 12 => b,
            _ => return stored.to_string(),
        };
        let nonce = Nonce::from_slice(&bytes[..12]);

        match cipher.decrypt(nonce, &bytes[12..]) {
            Ok(plaintext) => String::from_utf8(plaintext).unwrap_or_else(|_| stored.to_string()),
            Err(_) => stored.to_string(),
        }
    }

    fn build_llm_config(&self) -> LlmConfig {
        let api_key = self
            .env
            .secret("ANTHROPIC_API_KEY")
            .map(|s| s.to_string())
            .unwrap_or_default();

        let model = self
            .env
            .var("CLAUDE_MODEL")
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "claude-sonnet-4-20250514".to_string());

        let base_url = self
            .env
            .var("ANTHROPIC_BASE_URL")
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "https://api.anthropic.com".to_string());

        LlmConfig {
            api_key,
            model,
            base_url,
            max_tokens: 4096,
        }
    }

    fn load_messages(&self, limit: u32) -> Vec<Message> {
        let sql = self.state.storage().sql();
        let cursor = match sql.exec(
            "SELECT role, content, created_at FROM messages ORDER BY id DESC LIMIT ?",
            Some(vec![SqlStorageValue::Integer(limit as i64)]),
        ) {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let mut messages: Vec<Message> = cursor
            .raw()
            .filter_map(|row| {
                let values = row.ok()?;
                let role_str = match &values[0] {
                    SqlStorageValue::String(s) => s.clone(),
                    _ => return None,
                };
                let content_json = match &values[1] {
                    SqlStorageValue::String(s) => s.clone(),
                    _ => return None,
                };
                let created_at = match &values[2] {
                    SqlStorageValue::Integer(i) => *i,
                    SqlStorageValue::Float(f) => *f as i64,
                    _ => return None,
                };

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

        messages.reverse(); // DESC -> chronological order
        messages
    }

    fn persist_messages(&self, messages: &[Message]) {
        let sql = self.state.storage().sql();
        let now = js_sys::Date::now() as i64;
        for msg in messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let content_json = serde_json::to_string(&msg.content).unwrap_or_default();
            let bindings: Vec<SqlStorageValue> = vec![
                role.into(),
                content_json.into(),
                SqlStorageValue::Integer(now),
            ];
            let _ = sql.exec(
                "INSERT INTO messages (role, content, created_at) VALUES (?, ?, ?)",
                Some(bindings),
            );
        }
    }

    fn load_system_prompt(&self) -> String {
        let sql = self.state.storage().sql();
        let none: Option<Vec<SqlStorageValue>> = None;
        let cursor = match sql.exec("SELECT value FROM prefs WHERE key = 'system_prompt'", none) {
            Ok(c) => c,
            Err(_) => return "You are a helpful AI assistant.".to_string(),
        };

        cursor
            .raw()
            .filter_map(|row| {
                let values = row.ok()?;
                match &values[0] {
                    SqlStorageValue::String(s) => Some(s.clone()),
                    _ => None,
                }
            })
            .next()
            .unwrap_or_else(|| "You are a helpful AI assistant.".to_string())
    }

    // --- Skill management ---

    fn load_skills(&self) -> Vec<SkillRow> {
        let sql = self.state.storage().sql();
        let none: Option<Vec<SqlStorageValue>> = None;
        let cursor = match sql.exec(
            "SELECT name, url, tools, added_at, auth_header_name, auth_header_value FROM skills",
            none,
        ) {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        cursor
            .raw()
            .filter_map(|row| {
                let values = row.ok()?;
                let name = match &values[0] {
                    SqlStorageValue::String(s) => s.clone(),
                    _ => return None,
                };
                let url = match &values[1] {
                    SqlStorageValue::String(s) => s.clone(),
                    _ => return None,
                };
                let tools_json = match &values[2] {
                    SqlStorageValue::String(s) => s.clone(),
                    _ => return None,
                };
                let added_at = match &values[3] {
                    SqlStorageValue::Integer(i) => *i,
                    SqlStorageValue::Float(f) => *f as i64,
                    _ => return None,
                };
                let auth_header_name = match values.get(4) {
                    Some(SqlStorageValue::String(s)) => Some(s.clone()),
                    _ => None,
                };
                let auth_header_value = match values.get(5) {
                    Some(SqlStorageValue::String(s)) => Some(self.decrypt_secret(s)),
                    _ => None,
                };

                Some(SkillRow {
                    name,
                    url,
                    tools_json,
                    added_at,
                    auth_header_name,
                    auth_header_value,
                })
            })
            .collect()
    }

    fn persist_skill(&self, row: &SkillRow) {
        let sql = self.state.storage().sql();
        let bindings: Vec<SqlStorageValue> = vec![
            row.name.clone().into(),
            row.url.clone().into(),
            row.tools_json.clone().into(),
            SqlStorageValue::Integer(row.added_at),
            match &row.auth_header_name {
                Some(s) => SqlStorageValue::String(s.clone()),
                None => SqlStorageValue::Null,
            },
            match &row.auth_header_value {
                Some(s) => SqlStorageValue::String(self.encrypt_secret(s)),
                None => SqlStorageValue::Null,
            },
        ];
        let _ = sql.exec(
            "INSERT OR REPLACE INTO skills (name, url, tools, added_at, auth_header_name, auth_header_value) VALUES (?, ?, ?, ?, ?, ?)",
            Some(bindings),
        );
    }

    fn build_registry(
        &self,
    ) -> std::result::Result<SkillRegistry<WorkerFetchBackend>, agent_core::AgentError> {
        let rows = self.load_skills();
        SkillRegistry::from_rows(rows, || WorkerFetchBackend)
    }

    // --- Pending approvals ---

    fn persist_pending_approval(&self, tool_call: &ToolCall) {
        let sql = self.state.storage().sql();
        let now = js_sys::Date::now() as i64;
        let tc_json = serde_json::to_string(tool_call).unwrap_or_default();
        let bindings: Vec<SqlStorageValue> = vec![tc_json.into(), SqlStorageValue::Integer(now)];
        let _ = sql.exec(
            "INSERT INTO pending_approvals (tool_call, created_at) VALUES (?, ?)",
            Some(bindings),
        );
    }

    fn load_pending_approval(&self, id: i64) -> Option<ToolCall> {
        let sql = self.state.storage().sql();
        let cursor = sql
            .exec(
                "SELECT tool_call FROM pending_approvals WHERE id = ?",
                Some(vec![SqlStorageValue::Integer(id)]),
            )
            .ok()?;

        cursor
            .raw()
            .filter_map(|row| {
                let values = row.ok()?;
                match &values[0] {
                    SqlStorageValue::String(s) => serde_json::from_str(s).ok(),
                    _ => None,
                }
            })
            .next()
    }

    fn delete_pending_approval(&self, id: i64) {
        let sql = self.state.storage().sql();
        let _ = sql.exec(
            "DELETE FROM pending_approvals WHERE id = ?",
            Some(vec![SqlStorageValue::Integer(id)]),
        );
    }

    fn list_pending_approvals(&self) -> Vec<serde_json::Value> {
        let sql = self.state.storage().sql();
        let none: Option<Vec<SqlStorageValue>> = None;
        let cursor = match sql.exec(
            "SELECT id, tool_call, created_at FROM pending_approvals",
            none,
        ) {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        cursor
            .raw()
            .filter_map(|row| {
                let values = row.ok()?;
                let id = match &values[0] {
                    SqlStorageValue::Integer(i) => *i,
                    _ => return None,
                };
                let tool_call_json = match &values[1] {
                    SqlStorageValue::String(s) => s.clone(),
                    _ => return None,
                };
                let created_at = match &values[2] {
                    SqlStorageValue::Integer(i) => *i,
                    SqlStorageValue::Float(f) => *f as i64,
                    _ => return None,
                };
                Some(serde_json::json!({
                    "id": id,
                    "tool_call": serde_json::from_str::<serde_json::Value>(&tool_call_json).ok()?,
                    "created_at": created_at,
                }))
            })
            .collect()
    }

    /// Shared agent turn logic used by both HTTP and WebSocket handlers.
    /// Now includes skill-based tool execution loop with human-in-the-loop.
    async fn run_agent_turn(&self, user_message: &str) -> Result<serde_json::Value> {
        let config = self.build_llm_config();
        let llm = LlmClient::new(config, WorkerFetchBackend);
        let agent = Agent::new(llm);

        let messages = self.load_messages(50);
        let system_prompt = self.load_system_prompt();

        let registry = self
            .build_registry()
            .map_err(|e| Error::RustError(e.to_string()))?;
        let tools = registry.all_tools();

        let ctx = AgentContext {
            system_prompt,
            messages,
            tools,
        };

        let mut result: AgentRunResult = agent
            .run(ctx, user_message)
            .await
            .map_err(|e| Error::RustError(e.to_string()))?;

        // Persist the initial messages (user + assistant)
        self.persist_messages(&result.new_messages);

        // Tool execution loop: run → execute tools → resume, until no more tool calls
        while !result.pending_tool_calls.is_empty() {
            let (safe_calls, destructive_calls) = check_destructive(&result.pending_tool_calls);

            // Persist destructive calls as pending approvals
            if !destructive_calls.is_empty() {
                for tc in &destructive_calls {
                    self.persist_pending_approval(tc);
                }

                // If ALL calls are destructive, return early awaiting approval
                if safe_calls.is_empty() {
                    return Ok(serde_json::json!({
                        "status": "awaiting_approval",
                        "answer": result.answer,
                        "pending_approvals": destructive_calls,
                    }));
                }
            }

            // Execute safe tool calls via the registry
            let mut tool_results = Vec::new();
            for tc in &safe_calls {
                let tr = registry.execute(tc).await.unwrap_or_else(|e| ToolResult {
                    tool_use_id: tc.id.clone(),
                    content: format!("Tool execution error: {e}"),
                    is_error: true,
                });
                tool_results.push(tr);
            }

            // For destructive calls that were deferred, return error results so
            // the agent knows they weren't executed
            for tc in &destructive_calls {
                tool_results.push(ToolResult {
                    tool_use_id: tc.id.clone(),
                    content: "This tool call requires human approval and is pending.".to_string(),
                    is_error: true,
                });
            }

            // Rebuild context and resume
            let messages = self.load_messages(50);
            let system_prompt = self.load_system_prompt();
            let tools = registry.all_tools();

            let ctx = AgentContext {
                system_prompt,
                messages,
                tools,
            };

            let config = self.build_llm_config();
            let llm = LlmClient::new(config, WorkerFetchBackend);
            let agent = Agent::new(llm);

            result = agent
                .resume(ctx, tool_results)
                .await
                .map_err(|e| Error::RustError(e.to_string()))?;

            self.persist_messages(&result.new_messages);
        }

        Ok(serde_json::json!({
            "answer": result.answer,
            "pending_tool_calls": result.pending_tool_calls,
        }))
    }

    async fn handle_message(&self, mut req: Request) -> Result<Response> {
        #[derive(Deserialize)]
        struct MessageRequest {
            message: String,
        }

        let body: MessageRequest = req.json().await?;
        let response_body = self.run_agent_turn(&body.message).await?;
        Response::from_json(&response_body)
    }

    fn handle_history(&self) -> Result<Response> {
        let messages = self.load_messages(50);
        Response::from_json(&messages)
    }

    fn handle_websocket_upgrade(&self) -> Result<Response> {
        let pair = WebSocketPair::new()?;
        self.state.accept_web_socket(&pair.server);
        Response::from_websocket(pair.client)
    }

    // --- Skill management handlers ---

    async fn handle_add_skill(&self, mut req: Request) -> Result<Response> {
        #[derive(Deserialize)]
        struct AddSkillRequest {
            name: String,
            url: String,
            #[serde(default)]
            auth_header_name: Option<String>,
            #[serde(default)]
            auth_header_value: Option<String>,
        }

        let body: AddSkillRequest = req.json().await?;

        let mut registry = self
            .build_registry()
            .map_err(|e| Error::RustError(e.to_string()))?;

        let now = js_sys::Date::now() as i64;
        let row = registry
            .register(
                body.name,
                body.url,
                WorkerFetchBackend,
                now,
                body.auth_header_name,
                body.auth_header_value,
            )
            .await
            .map_err(|e| Error::RustError(e.to_string()))?;

        self.persist_skill(&row);

        let tools = registry.all_tools();
        Response::from_json(&serde_json::json!({
            "skill": row.name,
            "tools": tools.iter()
                .filter(|t| t.name.starts_with(&row.name))
                .map(|t| &t.name)
                .collect::<Vec<_>>(),
        }))
    }

    fn handle_list_skills(&self) -> Result<Response> {
        let mut rows = self.load_skills();
        for row in &mut rows {
            if let Some(ref value) = row.auth_header_value {
                row.auth_header_value = Some(mask_secret(value));
            }
        }
        Response::from_json(&rows)
    }

    // --- Approval handlers ---

    async fn handle_approve(&self, mut req: Request) -> Result<Response> {
        #[derive(Deserialize)]
        struct ApproveRequest {
            id: i64,
            #[serde(default)]
            approve: bool,
        }

        let body: ApproveRequest = req.json().await?;

        let tool_call = self
            .load_pending_approval(body.id)
            .ok_or_else(|| Error::RustError(format!("Pending approval {} not found", body.id)))?;

        self.delete_pending_approval(body.id);

        if !body.approve {
            return Response::from_json(&serde_json::json!({
                "status": "denied",
                "tool_call": tool_call,
            }))
            .map_err(|e| Error::RustError(e.to_string()));
        }

        // Execute the approved tool call
        let registry = self
            .build_registry()
            .map_err(|e| Error::RustError(e.to_string()))?;

        let tool_result = registry
            .execute(&tool_call)
            .await
            .unwrap_or_else(|e| ToolResult {
                tool_use_id: tool_call.id.clone(),
                content: format!("Tool execution error: {e}"),
                is_error: true,
            });

        // Resume the agent with the tool result
        let messages = self.load_messages(50);
        let system_prompt = self.load_system_prompt();
        let tools = registry.all_tools();

        let ctx = AgentContext {
            system_prompt,
            messages,
            tools,
        };

        let config = self.build_llm_config();
        let llm = LlmClient::new(config, WorkerFetchBackend);
        let agent = Agent::new(llm);

        let result = agent
            .resume(ctx, vec![tool_result])
            .await
            .map_err(|e| Error::RustError(e.to_string()))?;

        self.persist_messages(&result.new_messages);

        Response::from_json(&serde_json::json!({
            "status": "approved",
            "answer": result.answer,
            "pending_tool_calls": result.pending_tool_calls,
        }))
    }
}

impl DurableObject for AgentDo {
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            initialized: Cell::new(false),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.ensure_schema();

        let path = req.path();
        let method = req.method();

        match (method, path.as_str()) {
            (Method::Post, "/message") => self.handle_message(req).await,
            (Method::Get, "/history") => self.handle_history(),
            (Method::Post, "/skills/add") => self.handle_add_skill(req).await,
            (Method::Get, "/skills") => self.handle_list_skills(),
            (Method::Post, "/approve") => self.handle_approve(req).await,
            (Method::Get, "/approvals") => Response::from_json(&self.list_pending_approvals()),
            (Method::Get, "/") => {
                // Check for WebSocket upgrade
                let upgrade = req.headers().get("Upgrade").ok().flatten();
                if upgrade.as_deref() == Some("websocket") {
                    self.handle_websocket_upgrade()
                } else {
                    Response::error("Expected WebSocket upgrade", 426)
                }
            }
            _ => Response::error("Not Found", 404),
        }
    }

    async fn websocket_message(
        &self,
        ws: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> Result<()> {
        self.ensure_schema();

        let text = match message {
            WebSocketIncomingMessage::String(s) => s,
            WebSocketIncomingMessage::Binary(_) => {
                ws.send_with_str(r#"{"error":"binary messages not supported"}"#)?;
                return Ok(());
            }
        };

        // Try parsing as approve/deny message first
        #[derive(Deserialize)]
        struct WsApproval {
            #[serde(rename = "type")]
            msg_type: String,
            id: i64,
        }

        if let Ok(approval) = serde_json::from_str::<WsApproval>(&text) {
            if approval.msg_type == "approve" || approval.msg_type == "deny" {
                let approved = approval.msg_type == "approve";
                let result = self.handle_ws_approval(approval.id, approved).await;
                let response = match result {
                    Ok(val) => val,
                    Err(e) => serde_json::json!({ "error": e.to_string() }),
                };
                ws.send_with_str(serde_json::to_string(&response).unwrap_or_default())?;
                return Ok(());
            }
        }

        // Regular message
        #[derive(Deserialize)]
        struct WsMessage {
            message: String,
        }

        let parsed: WsMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                let err = serde_json::json!({ "error": format!("invalid JSON: {e}") });
                ws.send_with_str(serde_json::to_string(&err).unwrap_or_default())?;
                return Ok(());
            }
        };

        match self.run_agent_turn(&parsed.message).await {
            Ok(result) => {
                // If awaiting approval, send approval_required event
                if result.get("status").and_then(|v| v.as_str()) == Some("awaiting_approval") {
                    let approval_msg = serde_json::json!({
                        "type": "approval_required",
                        "pending_approvals": result["pending_approvals"],
                    });
                    ws.send_with_str(serde_json::to_string(&approval_msg).unwrap_or_default())?;
                } else {
                    ws.send_with_str(serde_json::to_string(&result).unwrap_or_default())?;
                }
            }
            Err(e) => {
                let err = serde_json::json!({ "error": e.to_string() });
                ws.send_with_str(serde_json::to_string(&err).unwrap_or_default())?;
            }
        }

        Ok(())
    }

    async fn websocket_close(
        &self,
        _ws: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }
}

impl AgentDo {
    async fn handle_ws_approval(&self, id: i64, approved: bool) -> Result<serde_json::Value> {
        let tool_call = self
            .load_pending_approval(id)
            .ok_or_else(|| Error::RustError(format!("Pending approval {id} not found")))?;

        self.delete_pending_approval(id);

        if !approved {
            return Ok(serde_json::json!({
                "type": "approval_result",
                "status": "denied",
                "tool_call": tool_call,
            }));
        }

        let registry = self
            .build_registry()
            .map_err(|e| Error::RustError(e.to_string()))?;

        let tool_result = registry
            .execute(&tool_call)
            .await
            .unwrap_or_else(|e| ToolResult {
                tool_use_id: tool_call.id.clone(),
                content: format!("Tool execution error: {e}"),
                is_error: true,
            });

        let messages = self.load_messages(50);
        let system_prompt = self.load_system_prompt();
        let tools = registry.all_tools();

        let ctx = AgentContext {
            system_prompt,
            messages,
            tools,
        };

        let config = self.build_llm_config();
        let llm = LlmClient::new(config, WorkerFetchBackend);
        let agent = Agent::new(llm);

        let result = agent
            .resume(ctx, vec![tool_result])
            .await
            .map_err(|e| Error::RustError(e.to_string()))?;

        self.persist_messages(&result.new_messages);

        Ok(serde_json::json!({
            "type": "approval_result",
            "status": "approved",
            "answer": result.answer,
            "pending_tool_calls": result.pending_tool_calls,
        }))
    }
}
