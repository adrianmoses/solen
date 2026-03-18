use std::cell::Cell;

use croner::Cron;
use mcp_server_util::{
    initialize_result, tool_call_result, tools_list_result, JsonRpcRequest, JsonRpcResponse,
    ToolDef,
};
use serde::Deserialize;
use worker::*;

fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "schedule_create",
            description: "Create a scheduled task. Provide either cron_expr (recurring) or run_at (one-shot epoch ms). Optionally specify tool_params to hint the agent to use a specific tool when the schedule fires.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "Prompt message to send to the agent when the schedule fires" },
                    "cron_expr": { "type": "string", "description": "Crontab expression for recurring schedules (e.g. '0 9 * * 1' for every Monday at 9am)" },
                    "run_at": { "type": "integer", "description": "Epoch milliseconds for one-shot schedule" },
                    "tool_params": {
                        "type": "object",
                        "description": "Optional hint to use a specific tool when the schedule fires",
                        "properties": {
                            "tool_name": { "type": "string" },
                            "tool_input": { "type": "object" }
                        }
                    }
                },
                "required": ["message"]
            }),
        },
        ToolDef {
            name: "schedule_list",
            description: "List all schedules, optionally filtered to enabled only",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "enabled_only": { "type": "boolean", "description": "If true, only return enabled schedules" }
                }
            }),
        },
        ToolDef {
            name: "schedule_get",
            description: "Get a schedule by ID",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Schedule ID" }
                },
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "schedule_delete",
            description: "Delete a schedule by ID",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Schedule ID" }
                },
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "schedule_update",
            description: "Update a schedule. Only provided fields are changed.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Schedule ID" },
                    "message": { "type": "string" },
                    "cron_expr": { "type": "string" },
                    "run_at": { "type": "integer" },
                    "enabled": { "type": "boolean" }
                },
                "required": ["id"]
            }),
        },
    ]
}

// --- Dispatcher ---

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    let method = req.method();

    let user_id = req
        .headers()
        .get("X-User-Id")
        .ok()
        .flatten()
        .unwrap_or_else(|| "default".to_string());

    let namespace = env.durable_object("SCHEDULE_DO")?;
    let stub = namespace
        .id_from_name(&format!("schedule:{user_id}"))?
        .get_stub()?;

    let is_valid = (method == Method::Post
        && (path.as_str() == "/mcp" || path.as_str() == "/schedules"))
        || (method == Method::Get && path.as_str() == "/schedules")
        || (method == Method::Delete && path.starts_with("/schedules/"));

    if is_valid {
        stub.fetch_with_request(req).await
    } else {
        Response::error("Not Found", 404)
    }
}

/// (id, message, cron_expr, run_at, tool_params)
type DueSchedule = (i64, String, Option<String>, Option<i64>, Option<String>);

// --- ScheduleDo Durable Object ---

#[durable_object]
pub struct ScheduleDo {
    state: State,
    env: Env,
    initialized: Cell<bool>,
}

impl ScheduleDo {
    fn ensure_schema(&self) {
        if self.initialized.get() {
            return;
        }
        let sql = self.state.storage().sql();
        let none: Option<Vec<SqlStorageValue>> = None;
        match sql.exec(
            "CREATE TABLE IF NOT EXISTS schedules (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                message     TEXT    NOT NULL,
                cron_expr   TEXT,
                run_at      INTEGER,
                next_run_at INTEGER NOT NULL,
                tool_params TEXT,
                enabled     INTEGER NOT NULL DEFAULT 1,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            )",
            none,
        ) {
            Ok(_) => self.initialized.set(true),
            Err(e) => {
                console_error!("Failed to initialize schedules schema: {:?}", e);
            }
        }
    }

    fn now_ms(&self) -> i64 {
        js_sys::Date::now() as i64
    }

    /// Compute the next occurrence from a cron expression after `after_ms` (epoch ms).
    fn next_cron_occurrence(&self, cron_expr: &str, after_ms: i64) -> Option<i64> {
        let cron = Cron::new(cron_expr).parse().ok()?;
        let after_secs = after_ms / 1000;
        let dt = chrono::DateTime::from_timestamp(after_secs, 0)?;
        let next = cron.find_next_occurrence(&dt, false).ok()?;
        Some(next.timestamp() * 1000)
    }

    /// Recalculate and set the DO alarm to the next soonest schedule.
    async fn refresh_alarm(&self) {
        let sql = self.state.storage().sql();
        let none: Option<Vec<SqlStorageValue>> = None;
        let cursor = match sql.exec(
            "SELECT MIN(next_run_at) FROM schedules WHERE enabled = 1",
            none,
        ) {
            Ok(c) => c,
            Err(_) => return,
        };

        let next_run =
            cursor
                .raw()
                .filter_map(|r| r.ok())
                .next()
                .and_then(|values| match &values[0] {
                    SqlStorageValue::Integer(i) => Some(*i),
                    SqlStorageValue::Float(f) => Some(*f as i64),
                    _ => None,
                });

        let storage = self.state.storage();
        match next_run {
            Some(ts) => {
                let _ = storage.set_alarm(ts).await;
            }
            None => {
                let _ = storage.delete_alarm().await;
            }
        }
    }

    /// Extract user_id from the DO name "schedule:{user_id}".
    fn user_id(&self) -> String {
        let id_str = self.state.id().to_string();
        // The DO was created with id_from_name("schedule:{user_id}")
        // We can't recover the name from the ID directly, so we store it
        // on first request via the X-User-Id header instead.
        // For alarm callbacks we need to retrieve it from SQLite.
        let sql = self.state.storage().sql();
        let none: Option<Vec<SqlStorageValue>> = None;
        let cursor = match sql.exec("SELECT value FROM meta WHERE key = 'user_id'", none) {
            Ok(c) => c,
            Err(_) => return id_str,
        };

        cursor
            .raw()
            .filter_map(|r| r.ok())
            .next()
            .and_then(|values| match &values[0] {
                SqlStorageValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or(id_str)
    }

    fn store_user_id(&self, user_id: &str) {
        let sql = self.state.storage().sql();
        let _ = sql.exec(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            None::<Vec<SqlStorageValue>>,
        );
        let _ = sql.exec(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('user_id', ?)",
            Some(vec![SqlStorageValue::String(user_id.to_string())]),
        );
    }

    // --- MCP Tool Handlers ---

    async fn handle_tool_call(&self, name: &str, params: &serde_json::Value) -> serde_json::Value {
        match name {
            "schedule_create" => self.tool_create(params).await,
            "schedule_list" => self.tool_list(params),
            "schedule_get" => self.tool_get(params),
            "schedule_delete" => self.tool_delete(params).await,
            "schedule_update" => self.tool_update(params).await,
            _ => tool_call_result(&format!("Unknown tool: {name}"), true),
        }
    }

    async fn tool_create(&self, params: &serde_json::Value) -> serde_json::Value {
        let message = match params["message"].as_str() {
            Some(m) => m,
            None => return tool_call_result("Missing required field 'message'", true),
        };
        let cron_expr = params.get("cron_expr").and_then(|v| v.as_str());
        let run_at = params.get("run_at").and_then(|v| v.as_i64());
        let tool_params = params.get("tool_params");

        if cron_expr.is_none() && run_at.is_none() {
            return tool_call_result(
                "Must provide either 'cron_expr' (recurring) or 'run_at' (one-shot epoch ms)",
                true,
            );
        }

        let now = self.now_ms();

        // Compute next_run_at
        let next_run_at = if let Some(cron) = cron_expr {
            match self.next_cron_occurrence(cron, now) {
                Some(ts) => ts,
                None => {
                    return tool_call_result(
                        &format!("Invalid or unparseable cron expression: '{cron}'"),
                        true,
                    )
                }
            }
        } else {
            run_at.unwrap() // safe: checked above
        };

        let tool_params_json = tool_params.map(|v| serde_json::to_string(v).unwrap_or_default());

        let sql = self.state.storage().sql();
        let bindings: Vec<SqlStorageValue> = vec![
            message.into(),
            match cron_expr {
                Some(c) => SqlStorageValue::String(c.to_string()),
                None => SqlStorageValue::Null,
            },
            match run_at {
                Some(r) => SqlStorageValue::Integer(r),
                None => SqlStorageValue::Null,
            },
            SqlStorageValue::Integer(next_run_at),
            match tool_params_json {
                Some(ref j) => SqlStorageValue::String(j.clone()),
                None => SqlStorageValue::Null,
            },
            SqlStorageValue::Integer(now),
            SqlStorageValue::Integer(now),
        ];

        match sql.exec(
            "INSERT INTO schedules (message, cron_expr, run_at, next_run_at, tool_params, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            Some(bindings),
        ) {
            Ok(_) => {
                self.refresh_alarm().await;
                let result = serde_json::json!({
                    "status": "created",
                    "next_run_at": next_run_at,
                    "cron_expr": cron_expr,
                    "run_at": run_at,
                });
                tool_call_result(&result.to_string(), false)
            }
            Err(e) => tool_call_result(&format!("Failed to create schedule: {e:?}"), true),
        }
    }

    fn tool_list(&self, params: &serde_json::Value) -> serde_json::Value {
        let enabled_only = params
            .get("enabled_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let sql = self.state.storage().sql();
        let cursor = if enabled_only {
            sql.exec(
                "SELECT id, message, cron_expr, run_at, next_run_at, tool_params, enabled, created_at, updated_at FROM schedules WHERE enabled = 1",
                None::<Vec<SqlStorageValue>>,
            )
        } else {
            sql.exec(
                "SELECT id, message, cron_expr, run_at, next_run_at, tool_params, enabled, created_at, updated_at FROM schedules",
                None::<Vec<SqlStorageValue>>,
            )
        };

        let cursor = match cursor {
            Ok(c) => c,
            Err(e) => return tool_call_result(&format!("Query failed: {e:?}"), true),
        };

        let entries: Vec<serde_json::Value> = cursor
            .raw()
            .filter_map(|r| Some(Self::row_to_json(&r.ok()?)))
            .collect();

        tool_call_result(&serde_json::to_string(&entries).unwrap_or_default(), false)
    }

    fn tool_get(&self, params: &serde_json::Value) -> serde_json::Value {
        let id = match params["id"].as_i64() {
            Some(i) => i,
            None => return tool_call_result("Missing required field 'id'", true),
        };

        let sql = self.state.storage().sql();
        let cursor = match sql.exec(
            "SELECT id, message, cron_expr, run_at, next_run_at, tool_params, enabled, created_at, updated_at FROM schedules WHERE id = ?",
            Some(vec![SqlStorageValue::Integer(id)]),
        ) {
            Ok(c) => c,
            Err(e) => return tool_call_result(&format!("Query failed: {e:?}"), true),
        };

        match cursor.raw().filter_map(|r| r.ok()).next() {
            Some(values) => {
                let row = Self::row_to_json(&values);
                tool_call_result(&row.to_string(), false)
            }
            None => tool_call_result(&format!("Schedule with id {id} not found"), true),
        }
    }

    async fn tool_delete(&self, params: &serde_json::Value) -> serde_json::Value {
        let id = match params["id"].as_i64() {
            Some(i) => i,
            None => return tool_call_result("Missing required field 'id'", true),
        };

        let sql = self.state.storage().sql();
        match sql.exec(
            "DELETE FROM schedules WHERE id = ?",
            Some(vec![SqlStorageValue::Integer(id)]),
        ) {
            Ok(_) => {
                self.refresh_alarm().await;
                tool_call_result(&format!("Deleted schedule {id}"), false)
            }
            Err(e) => tool_call_result(&format!("Failed to delete: {e:?}"), true),
        }
    }

    async fn tool_update(&self, params: &serde_json::Value) -> serde_json::Value {
        let id = match params["id"].as_i64() {
            Some(i) => i,
            None => return tool_call_result("Missing required field 'id'", true),
        };

        let now = self.now_ms();
        let sql = self.state.storage().sql();

        // Load existing schedule
        let cursor = match sql.exec(
            "SELECT message, cron_expr, run_at, enabled FROM schedules WHERE id = ?",
            Some(vec![SqlStorageValue::Integer(id)]),
        ) {
            Ok(c) => c,
            Err(e) => return tool_call_result(&format!("Query failed: {e:?}"), true),
        };

        let existing = match cursor.raw().filter_map(|r| r.ok()).next() {
            Some(v) => v,
            None => return tool_call_result(&format!("Schedule {id} not found"), true),
        };

        let cur_cron = match &existing[1] {
            SqlStorageValue::String(s) => Some(s.clone()),
            _ => None,
        };
        let cur_run_at = match &existing[2] {
            SqlStorageValue::Integer(i) => Some(*i),
            _ => None,
        };

        // Apply updates
        let new_message = params.get("message").and_then(|v| v.as_str());
        let new_cron = params.get("cron_expr").and_then(|v| v.as_str());
        let new_run_at = params.get("run_at").and_then(|v| v.as_i64());
        let new_enabled = params.get("enabled").and_then(|v| v.as_bool());

        // Recompute next_run_at if cron or run_at changed
        let effective_cron = new_cron.map(|s| s.to_string()).or(cur_cron);
        let effective_run_at = new_run_at.or(cur_run_at);

        let next_run_at = if new_cron.is_some() || new_run_at.is_some() {
            if let Some(ref cron) = effective_cron {
                match self.next_cron_occurrence(cron, now) {
                    Some(ts) => Some(ts),
                    None => {
                        return tool_call_result(
                            &format!("Invalid cron expression: '{cron}'"),
                            true,
                        )
                    }
                }
            } else {
                effective_run_at
            }
        } else {
            None // no schedule timing change
        };

        // Build dynamic UPDATE
        let mut sets = vec!["updated_at = ?"];
        let mut bindings: Vec<SqlStorageValue> = vec![SqlStorageValue::Integer(now)];

        if let Some(msg) = new_message {
            sets.push("message = ?");
            bindings.push(msg.into());
        }
        if let Some(cron) = new_cron {
            sets.push("cron_expr = ?");
            bindings.push(SqlStorageValue::String(cron.to_string()));
        }
        if let Some(ra) = new_run_at {
            sets.push("run_at = ?");
            bindings.push(SqlStorageValue::Integer(ra));
        }
        if let Some(nra) = next_run_at {
            sets.push("next_run_at = ?");
            bindings.push(SqlStorageValue::Integer(nra));
        }
        if let Some(en) = new_enabled {
            sets.push("enabled = ?");
            bindings.push(SqlStorageValue::Integer(i64::from(en)));
        }

        bindings.push(SqlStorageValue::Integer(id));
        let query = format!("UPDATE schedules SET {} WHERE id = ?", sets.join(", "));

        match sql.exec(&query, Some(bindings)) {
            Ok(_) => {
                self.refresh_alarm().await;
                tool_call_result(&format!("Updated schedule {id}"), false)
            }
            Err(e) => tool_call_result(&format!("Failed to update: {e:?}"), true),
        }
    }

    /// Convert a raw SQL row to JSON.
    fn row_to_json(values: &[SqlStorageValue]) -> serde_json::Value {
        let get_int = |idx: usize| match &values[idx] {
            SqlStorageValue::Integer(i) => Some(*i),
            SqlStorageValue::Float(f) => Some(*f as i64),
            _ => None,
        };
        let get_str = |idx: usize| match &values[idx] {
            SqlStorageValue::String(s) => Some(s.clone()),
            _ => None,
        };

        serde_json::json!({
            "id": get_int(0),
            "message": get_str(1).unwrap_or_default(),
            "cron_expr": get_str(2),
            "run_at": get_int(3),
            "next_run_at": get_int(4),
            "tool_params": get_str(5).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
            "enabled": get_int(6).map(|i| i == 1).unwrap_or(true),
            "created_at": get_int(7),
            "updated_at": get_int(8),
        })
    }

    // --- REST Handlers ---

    async fn handle_rest_create(&self, mut req: Request) -> Result<Response> {
        #[derive(Deserialize)]
        struct CreateRequest {
            message: String,
            #[serde(default)]
            cron_expr: Option<String>,
            #[serde(default)]
            run_at: Option<i64>,
            #[serde(default)]
            tool_params: Option<serde_json::Value>,
        }

        let body: CreateRequest = req.json().await?;
        let params = serde_json::json!({
            "message": body.message,
            "cron_expr": body.cron_expr,
            "run_at": body.run_at,
            "tool_params": body.tool_params,
        });
        let result = self.tool_create(&params).await;
        Response::from_json(&result)
    }

    fn handle_rest_list(&self) -> Result<Response> {
        let params = serde_json::json!({ "enabled_only": false });
        let result = self.tool_list(&params);
        Response::from_json(&result)
    }

    async fn handle_rest_delete(&self, path: &str) -> Result<Response> {
        let id_str = path.strip_prefix("/schedules/").unwrap_or("");
        let id: i64 = id_str
            .parse()
            .map_err(|_| Error::RustError(format!("Invalid schedule id: {id_str}")))?;
        let params = serde_json::json!({ "id": id });
        let result = self.tool_delete(&params).await;
        Response::from_json(&result)
    }

    // --- Alarm callback ---

    async fn process_due_schedules(&self) -> Result<()> {
        let now = self.now_ms();
        let sql = self.state.storage().sql();

        let cursor = match sql.exec(
            "SELECT id, message, cron_expr, run_at, tool_params FROM schedules WHERE next_run_at <= ? AND enabled = 1",
            Some(vec![SqlStorageValue::Integer(now)]),
        ) {
            Ok(c) => c,
            Err(e) => {
                console_error!("Failed to query due schedules: {:?}", e);
                return Ok(());
            }
        };

        let due_schedules: Vec<DueSchedule> = cursor
            .raw()
            .filter_map(|r| {
                let values = r.ok()?;
                let id = match &values[0] {
                    SqlStorageValue::Integer(i) => *i,
                    _ => return None,
                };
                let message = match &values[1] {
                    SqlStorageValue::String(s) => s.clone(),
                    _ => return None,
                };
                let cron_expr = match &values[2] {
                    SqlStorageValue::String(s) => Some(s.clone()),
                    _ => None,
                };
                let run_at = match &values[3] {
                    SqlStorageValue::Integer(i) => Some(*i),
                    _ => None,
                };
                let tool_params = match &values[4] {
                    SqlStorageValue::String(s) => Some(s.clone()),
                    _ => None,
                };
                Some((id, message, cron_expr, run_at, tool_params))
            })
            .collect();

        let user_id = self.user_id();

        for (id, message, cron_expr, _run_at, tool_params) in &due_schedules {
            // Callback to AgentDO via service binding
            let edgeclaw = match self.env.service("EDGECLAW") {
                Ok(s) => s,
                Err(e) => {
                    console_error!("EDGECLAW service binding not available: {:?}", e);
                    continue;
                }
            };

            let mut body = serde_json::json!({ "message": message });
            if let Some(tp) = tool_params {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(tp) {
                    body["tool_params"] = parsed;
                }
            }

            let mut init = RequestInit::new();
            init.method = Method::Post;
            init.body = Some(wasm_bindgen::JsValue::from_str(
                &serde_json::to_string(&body).unwrap_or_default(),
            ));
            init.headers
                .set("content-type", "application/json")
                .unwrap_or(());
            init.headers.set("X-User-Id", &user_id).unwrap_or(());

            let callback_req = match Request::new_with_init("https://fake-host/message", &init) {
                Ok(r) => r,
                Err(e) => {
                    console_error!("Failed to build callback request: {:?}", e);
                    continue;
                }
            };

            if let Err(e) = edgeclaw.fetch_request(callback_req).await {
                console_error!("Alarm callback failed for schedule {}: {:?}", id, e);
            }

            // Update or disable the schedule
            if let Some(cron) = cron_expr {
                // Recurring: compute next occurrence
                if let Some(next) = self.next_cron_occurrence(cron, now) {
                    let _ = sql.exec(
                        "UPDATE schedules SET next_run_at = ?, updated_at = ? WHERE id = ?",
                        Some(vec![
                            SqlStorageValue::Integer(next),
                            SqlStorageValue::Integer(now),
                            SqlStorageValue::Integer(*id),
                        ]),
                    );
                } else {
                    // Cron expression no longer valid, disable
                    let _ = sql.exec(
                        "UPDATE schedules SET enabled = 0, updated_at = ? WHERE id = ?",
                        Some(vec![
                            SqlStorageValue::Integer(now),
                            SqlStorageValue::Integer(*id),
                        ]),
                    );
                }
            } else {
                // One-shot: disable after firing
                let _ = sql.exec(
                    "UPDATE schedules SET enabled = 0, updated_at = ? WHERE id = ?",
                    Some(vec![
                        SqlStorageValue::Integer(now),
                        SqlStorageValue::Integer(*id),
                    ]),
                );
            }
        }

        self.refresh_alarm().await;
        Ok(())
    }
}

#[derive(Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

impl DurableObject for ScheduleDo {
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            initialized: Cell::new(false),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        self.ensure_schema();

        // Store user_id from header for alarm callbacks
        if let Some(user_id) = req.headers().get("X-User-Id").ok().flatten() {
            self.store_user_id(&user_id);
        }

        let path = req.path();
        let method = req.method();

        if method == Method::Post && path.as_str() == "/mcp" {
            let body: JsonRpcRequest = req
                .json()
                .await
                .map_err(|e| Error::RustError(format!("Invalid JSON-RPC: {e:?}")))?;

            let tools = tool_definitions();
            let response = match body.method.as_str() {
                "initialize" => {
                    JsonRpcResponse::success(body.id, initialize_result("skill-schedule", &tools))
                }
                "tools/list" => JsonRpcResponse::success(body.id, tools_list_result(&tools)),
                "tools/call" => {
                    let params: ToolCallParams = body
                        .params
                        .as_ref()
                        .and_then(|p| serde_json::from_value(p.clone()).ok())
                        .ok_or_else(|| Error::RustError("Missing tools/call params".to_string()))?;

                    let result = self.handle_tool_call(&params.name, &params.arguments).await;
                    JsonRpcResponse::success(body.id, result)
                }
                _ => JsonRpcResponse::method_not_found(body.id, &body.method),
            };

            Response::from_json(&response)
        } else if method == Method::Post && path.as_str() == "/schedules" {
            self.handle_rest_create(req).await
        } else if method == Method::Get && path.as_str() == "/schedules" {
            self.handle_rest_list()
        } else if method == Method::Delete && path.starts_with("/schedules/") {
            self.handle_rest_delete(path.as_str()).await
        } else {
            Response::error("Not Found", 404)
        }
    }

    async fn alarm(&self) -> Result<Response> {
        self.ensure_schema();
        self.process_due_schedules().await?;
        Response::ok("ok")
    }
}
