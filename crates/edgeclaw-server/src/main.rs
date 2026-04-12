use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_core::ReqwestBackend;
use anyhow::Result;
use skill_registry::SkillRegistry;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use edgeclaw_server::oauth;
use edgeclaw_server::scheduler::Scheduler;
use edgeclaw_server::server::{build_router, AppState, ServerConfig};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    dotenvy::dotenv().ok();

    let config = ServerConfig::from_env();
    let bind_addr = config.bind_addr();

    let connect_options: SqliteConnectOptions = config
        .database_url
        .parse::<SqliteConnectOptions>()?
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options)
        .await?;

    sqlx::migrate!().run(&pool).await?;
    tracing::info!("database ready");

    // Auto-register configured MCP skills
    if !config.skill_configs.is_empty() {
        auto_register_skills(&pool, &config).await;
    }

    let config = Arc::new(config);

    // Start background scheduler
    let sched = Scheduler::new(pool.clone(), config.clone());
    sched.start();

    // Initialize OAuth flow state and cleanup
    let oauth_flows = Arc::new(Mutex::new(HashMap::new()));
    oauth::spawn_flow_cleanup(oauth_flows.clone());

    let state = AppState {
        db: pool,
        config,
        oauth_flows,
        sessions: edgeclaw_server::session::new_registry(),
    };

    let app = build_router(state);
    let listener = TcpListener::bind(&bind_addr).await?;
    tracing::info!("listening on {}", bind_addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Base directory for skill SKILL.md files.
/// Defaults to `./skills` (matches Dockerfile WORKDIR), overridable via `SKILLS_DIR`.
fn skills_base_dir() -> std::path::PathBuf {
    std::env::var("SKILLS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("skills"))
}

/// Load SKILL.md content for a skill by searching known skill directories.
fn load_skill_context(skill_name: &str) -> Option<String> {
    let base = skills_base_dir();

    // Map skill names to SKILL.md file paths relative to the skills base dir
    let skill_dirs: &[(&str, &[&str])] = &[
        (
            "google_workspace",
            &["skill-gmail/SKILL.md", "skill-google-calendar/SKILL.md"],
        ),
        ("github", &["skill-github/SKILL.md"]),
    ];

    let mut context = String::new();

    for (name, paths) in skill_dirs {
        if skill_name == *name || skill_name.contains(name) {
            for rel_path in *paths {
                let full_path = base.join(rel_path);
                if let Ok(content) = std::fs::read_to_string(&full_path) {
                    if !context.is_empty() {
                        context.push_str("\n\n---\n\n");
                    }
                    context.push_str(&content);
                }
            }
        }
    }

    if context.is_empty() {
        None
    } else {
        Some(context)
    }
}

/// Auto-register MCP skills from SKILL_*_URL env vars on startup.
async fn auto_register_skills(pool: &SqlitePool, config: &ServerConfig) {
    let user_id = &config.default_user_id;

    // Ensure user exists
    let now = chrono::Utc::now().timestamp_millis();
    let _ = sqlx::query("INSERT OR IGNORE INTO users (id, created_at) VALUES (?, ?)")
        .bind(user_id)
        .bind(now)
        .execute(pool)
        .await;

    for skill_config in &config.skill_configs {
        let mut registered = false;

        for attempt in 1..=3 {
            tracing::info!(
                "auto-registering skill '{}' from {} (attempt {}/3)",
                skill_config.name,
                skill_config.url,
                attempt
            );

            let mut registry =
                SkillRegistry::<ReqwestBackend>::from_rows(vec![], ReqwestBackend::new).unwrap();

            let auth_header_name = skill_config
                .auth_token
                .as_ref()
                .map(|_| "authorization".to_string());
            let auth_header_value = skill_config
                .auth_token
                .as_ref()
                .map(|t| format!("Bearer {t}"));

            match registry
                .register(
                    skill_config.name.clone(),
                    skill_config.url.clone(),
                    ReqwestBackend::new(),
                    now,
                    auth_header_name,
                    auth_header_value,
                )
                .await
            {
                Ok(mut row) => {
                    // Load SKILL.md context
                    row.skill_context = load_skill_context(&skill_config.name);

                    // Upsert into skills table
                    let result = sqlx::query(
                        "INSERT OR REPLACE INTO skills (user_id, name, url, tools, added_at, skill_context, auth_header_name, auth_header_value, session_id) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(user_id)
                    .bind(&row.name)
                    .bind(&row.url)
                    .bind(&row.tools_json)
                    .bind(row.added_at)
                    .bind(&row.skill_context)
                    .bind(&row.auth_header_name)
                    .bind(&row.auth_header_value)
                    .bind(&row.session_id)
                    .execute(pool)
                    .await;

                    match result {
                        Ok(_) => {
                            let tool_count = registry.all_tools().len();
                            tracing::info!(
                                "registered skill '{}' with {} tools",
                                skill_config.name,
                                tool_count
                            );
                            registered = true;
                            break;
                        }
                        Err(e) => {
                            tracing::error!("failed to persist skill '{}': {e}", skill_config.name);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "failed to register skill '{}' (attempt {}/3): {e}",
                        skill_config.name,
                        attempt
                    );
                    if attempt < 3 {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        }

        if !registered {
            tracing::error!(
                "could not auto-register skill '{}' after 3 attempts — skipping",
                skill_config.name
            );
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
