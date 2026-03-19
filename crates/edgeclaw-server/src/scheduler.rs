use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;
use sqlx::SqlitePool;

use crate::server::ServerConfig;

pub struct Scheduler {
    pool: SqlitePool,
    config: Arc<ServerConfig>,
}

impl Scheduler {
    pub fn new(pool: SqlitePool, config: Arc<ServerConfig>) -> Self {
        Self { pool, config }
    }

    /// Spawn a background tokio task that polls scheduled_tasks every 10s.
    pub fn start(self) {
        tokio::spawn(async move {
            tracing::info!("scheduler started, polling every 10s");
            loop {
                if let Err(e) = self.poll_once().await {
                    tracing::error!("scheduler poll error: {e}");
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });
    }

    async fn poll_once(&self) -> Result<(), anyhow::Error> {
        let now = Utc::now().timestamp_millis();

        let tasks = sqlx::query_as::<_, (i64, String, String, Option<String>, String)>(
            "SELECT id, user_id, name, cron, payload FROM scheduled_tasks WHERE run_at <= ? AND enabled = 1",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;

        for (task_id, user_id, task_name, cron_expr, payload) in tasks {
            tracing::info!("executing scheduled task '{task_name}' for user '{user_id}'");

            // Parse payload for message
            let payload_json: serde_json::Value =
                serde_json::from_str(&payload).unwrap_or_default();
            let message = payload_json
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Scheduled task triggered")
                .to_string();

            // Run agent turn (run_agent_turn already handles the !Send bridge internally)
            match crate::agent::run_agent_turn(&self.pool, &self.config, &user_id, &message, None)
                .await
            {
                Ok(_) => {
                    tracing::info!("scheduled task '{task_name}' completed for user '{user_id}'");
                }
                Err(e) => {
                    tracing::error!(
                        "scheduled task '{task_name}' failed for user '{user_id}': {e}"
                    );
                }
            }

            // Update task: compute next run or disable
            let now_ts = Utc::now().timestamp_millis();
            if let Some(ref expr) = cron_expr {
                match cron::Schedule::from_str(expr) {
                    Ok(schedule) => {
                        if let Some(next) = schedule.upcoming(Utc).next() {
                            let next_ts = next.timestamp_millis();
                            sqlx::query(
                                "UPDATE scheduled_tasks SET run_at = ?, last_run = ? WHERE id = ?",
                            )
                            .bind(next_ts)
                            .bind(now_ts)
                            .bind(task_id)
                            .execute(&self.pool)
                            .await?;
                        } else {
                            sqlx::query(
                                "UPDATE scheduled_tasks SET enabled = 0, last_run = ? WHERE id = ?",
                            )
                            .bind(now_ts)
                            .bind(task_id)
                            .execute(&self.pool)
                            .await?;
                        }
                    }
                    Err(e) => {
                        tracing::error!("invalid cron expression '{expr}': {e}");
                        sqlx::query(
                            "UPDATE scheduled_tasks SET enabled = 0, last_run = ? WHERE id = ?",
                        )
                        .bind(now_ts)
                        .bind(task_id)
                        .execute(&self.pool)
                        .await?;
                    }
                }
            } else {
                // One-shot: disable after execution
                sqlx::query("UPDATE scheduled_tasks SET enabled = 0, last_run = ? WHERE id = ?")
                    .bind(now_ts)
                    .bind(task_id)
                    .execute(&self.pool)
                    .await?;
            }
        }

        Ok(())
    }
}
