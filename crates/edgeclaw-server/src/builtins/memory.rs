use agent_core::{BuiltinTool, ToolDefinition, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::agent::now_millis;

fn format_facts(facts: &[(String, String, Option<String>)]) -> ToolResult {
    if facts.is_empty() {
        return ToolResult::ok("No facts found.");
    }
    let content = facts
        .iter()
        .map(|(k, v, tags)| {
            let tag_str = tags
                .as_deref()
                .map(|t| format!(" [tags: {t}]"))
                .unwrap_or_default();
            format!("- {k}: {v}{tag_str}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    ToolResult::ok(content)
}

pub struct MemoryStoreTool {
    pool: SqlitePool,
    user_id: String,
}

impl MemoryStoreTool {
    pub fn new(pool: SqlitePool, user_id: String) -> Self {
        Self { pool, user_id }
    }
}

#[async_trait]
impl BuiltinTool for MemoryStoreTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_store".into(),
            description: "Store a fact in memory (key-value with optional comma-separated tags). Overwrites if key already exists.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The key to store the fact under"
                    },
                    "value": {
                        "type": "string",
                        "description": "The fact content"
                    },
                    "tags": {
                        "type": "string",
                        "description": "Optional comma-separated tags (e.g., 'preference,context')"
                    }
                },
                "required": ["key", "value"]
            }),
        }
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let key = match ToolResult::require_str(&input, "key") {
            Ok(k) => k.to_string(),
            Err(e) => return e,
        };
        let value = match ToolResult::require_str(&input, "value") {
            Ok(v) => v.to_string(),
            Err(e) => return e,
        };
        let tags = input.get("tags").and_then(|v| v.as_str()).map(String::from);
        let now = now_millis();

        let result = sqlx::query(
            "INSERT INTO memory_facts (user_id, key, value, tags, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(user_id, key) DO UPDATE SET value = excluded.value, tags = excluded.tags, created_at = excluded.created_at",
        )
        .bind(&self.user_id)
        .bind(&key)
        .bind(&value)
        .bind(tags.as_deref())
        .bind(now)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => ToolResult::ok(format!("Stored fact: {key}")),
            Err(e) => ToolResult::err(format!("Failed to store fact: {e}")),
        }
    }
}

pub struct MemoryFetchTool {
    pool: SqlitePool,
    user_id: String,
}

impl MemoryFetchTool {
    pub fn new(pool: SqlitePool, user_id: String) -> Self {
        Self { pool, user_id }
    }
}

#[async_trait]
impl BuiltinTool for MemoryFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_fetch".into(),
            description: "Retrieve a fact by key, or search by tag.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Exact key to look up"
                    },
                    "tag": {
                        "type": "string",
                        "description": "Search for facts containing this tag"
                    }
                }
            }),
        }
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let key = input.get("key").and_then(|v| v.as_str());
        let tag = input.get("tag").and_then(|v| v.as_str());

        let rows = if let Some(key) = key {
            sqlx::query_as::<_, (String, String, Option<String>)>(
                "SELECT key, value, tags FROM memory_facts WHERE user_id = ? AND key = ?",
            )
            .bind(&self.user_id)
            .bind(key)
            .fetch_all(&self.pool)
            .await
        } else if let Some(tag) = tag {
            let pattern = format!("%{tag}%");
            sqlx::query_as::<_, (String, String, Option<String>)>(
                "SELECT key, value, tags FROM memory_facts WHERE user_id = ? AND tags LIKE ?",
            )
            .bind(&self.user_id)
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await
        } else {
            return ToolResult::err("Provide either 'key' or 'tag' to search.");
        };

        match rows {
            Ok(facts) => format_facts(&facts),
            Err(e) => ToolResult::err(format!("Failed to fetch facts: {e}")),
        }
    }
}

pub struct MemoryListTool {
    pool: SqlitePool,
    user_id: String,
}

impl MemoryListTool {
    pub fn new(pool: SqlitePool, user_id: String) -> Self {
        Self { pool, user_id }
    }
}

#[async_trait]
impl BuiltinTool for MemoryListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_list".into(),
            description: "List all stored facts, optionally filtered by tag.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "tag": {
                        "type": "string",
                        "description": "Optional tag to filter by"
                    }
                }
            }),
        }
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let tag = input.get("tag").and_then(|v| v.as_str());

        let rows = if let Some(tag) = tag {
            let pattern = format!("%{tag}%");
            sqlx::query_as::<_, (String, String, Option<String>)>(
                "SELECT key, value, tags FROM memory_facts WHERE user_id = ? AND tags LIKE ? ORDER BY created_at DESC LIMIT 100",
            )
            .bind(&self.user_id)
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as::<_, (String, String, Option<String>)>(
                "SELECT key, value, tags FROM memory_facts WHERE user_id = ? ORDER BY created_at DESC LIMIT 100",
            )
            .bind(&self.user_id)
            .fetch_all(&self.pool)
            .await
        };

        match rows {
            Ok(facts) => {
                if facts.is_empty() {
                    ToolResult::ok("No facts stored.")
                } else {
                    format_facts(&facts)
                }
            }
            Err(e) => ToolResult::err(format!("Failed to list facts: {e}")),
        }
    }
}

pub struct MemoryDeleteTool {
    pool: SqlitePool,
    user_id: String,
}

impl MemoryDeleteTool {
    pub fn new(pool: SqlitePool, user_id: String) -> Self {
        Self { pool, user_id }
    }
}

#[async_trait]
impl BuiltinTool for MemoryDeleteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_delete".into(),
            description: "Delete a stored fact by key.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The key of the fact to delete"
                    }
                },
                "required": ["key"]
            }),
        }
    }

    fn needs_approval(&self, _input: &Value) -> bool {
        false
    }

    fn is_concurrent_safe(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let key = match ToolResult::require_str(&input, "key") {
            Ok(k) => k.to_string(),
            Err(e) => return e,
        };

        let result = sqlx::query("DELETE FROM memory_facts WHERE user_id = ? AND key = ?")
            .bind(&self.user_id)
            .bind(&key)
            .execute(&self.pool)
            .await;

        match result {
            Ok(r) => {
                if r.rows_affected() == 0 {
                    ToolResult::ok(format!("No fact found with key '{key}'"))
                } else {
                    ToolResult::ok(format!("Deleted fact: {key}"))
                }
            }
            Err(e) => ToolResult::err(format!("Failed to delete fact: {e}")),
        }
    }
}
