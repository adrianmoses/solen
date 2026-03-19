use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt;

use edgeclaw_server::scheduler::Scheduler;
use edgeclaw_server::server::{build_router, AppState, ServerConfig};

// --- Test helpers ---

fn test_config(mock_api_url: &str) -> ServerConfig {
    ServerConfig {
        database_url: "sqlite::memory:".to_string(),
        host: "127.0.0.1".to_string(),
        port: 0,
        anthropic_api_key: Some("test-key".to_string()),
        default_model: Some("test-model".to_string()),
        anthropic_base_url: mock_api_url.to_string(),
        max_tasks_per_user: 20,
    }
}

async fn test_app(mock_api_url: &str) -> axum::Router {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("failed to create in-memory pool");

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("failed to run migrations");

    let config = Arc::new(test_config(mock_api_url));
    let state = AppState { db: pool, config };
    build_router(state)
}

/// Start a mock Anthropic API server that returns responses from a sequence.
/// Each POST /v1/messages increments a counter and returns the corresponding response.
/// If the counter exceeds the list, it wraps to the last response.
async fn mock_anthropic_server(responses: Vec<&'static str>) -> String {
    use axum::extract::State;
    use axum::routing::post;
    use axum::Router;
    use tokio::net::TcpListener;

    #[derive(Clone)]
    struct MockState {
        responses: Vec<&'static str>,
        counter: Arc<AtomicUsize>,
    }

    async fn handler(State(state): State<MockState>) -> axum::Json<serde_json::Value> {
        let idx = state.counter.fetch_add(1, Ordering::Relaxed);
        let resp_str = if idx < state.responses.len() {
            state.responses[idx]
        } else {
            state.responses.last().unwrap()
        };
        axum::Json(serde_json::from_str(resp_str).unwrap())
    }

    let mock_state = MockState {
        responses,
        counter: Arc::new(AtomicUsize::new(0)),
    };

    let app = Router::new()
        .route("/v1/messages", post(handler))
        .with_state(mock_state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    url
}

/// Start a mock MCP skill server that responds to initialize, tools/list, and tools/call.
async fn mock_skill_server() -> String {
    use axum::routing::post;
    use axum::Router;
    use tokio::net::TcpListener;

    let app = Router::new().route(
        "/mcp",
        post(
            |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let id = body.get("id").and_then(|v| v.as_u64()).unwrap_or(1);
                let method = body.get("method").and_then(|v| v.as_str()).unwrap_or("");

                let result = match method {
                    "initialize" => serde_json::json!({
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "test-skill", "version": "0.1.0" }
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [{
                            "name": "greet",
                            "description": "Returns a greeting",
                            "inputSchema": { "type": "object", "properties": {} }
                        }]
                    }),
                    "tools/call" => serde_json::json!({
                        "content": [{ "type": "text", "text": "Hello from skill!" }],
                        "isError": false
                    }),
                    _ => serde_json::json!(null),
                };

                axum::Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                }))
            },
        ),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    url
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn delete_req(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

async fn test_app_with_state(mock_api_url: &str) -> (AppState, axum::Router) {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("failed to create in-memory pool");

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("failed to run migrations");

    let config = Arc::new(test_config(mock_api_url));
    let state = AppState {
        db: pool,
        config: config.clone(),
    };
    let router = build_router(state.clone());
    (state, router)
}

const END_TURN: &str = include_str!("../../../tests/fixtures/end_turn_response.json");
const TOOL_USE: &str = include_str!("../../../tests/fixtures/tool_use_response.json");

/// Wait for a spawned task to set last_run (up to 5s).
async fn wait_for_last_run(pool: &sqlx::SqlitePool, task_id: i64) -> i64 {
    for _ in 0..50 {
        let (last_run,): (Option<i64>,) =
            sqlx::query_as("SELECT last_run FROM scheduled_tasks WHERE id = ?")
                .bind(task_id)
                .fetch_one(pool)
                .await
                .unwrap();
        if let Some(ts) = last_run {
            return ts;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("last_run was not set within timeout for task {task_id}");
}

// --- Tests ---

#[tokio::test]
async fn test_health() {
    let app = test_app("http://unused").await;
    let resp = app.oneshot(get("/health")).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn test_message_end_turn() {
    let mock_url = mock_anthropic_server(vec![END_TURN]).await;
    let app = test_app(&mock_url).await;

    let resp = app
        .oneshot(post_json(
            "/message",
            serde_json::json!({
                "user_id": "test:1",
                "message": "Hi there"
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["answer"], "Hello! How can I help you today?");
}

#[tokio::test]
async fn test_message_creates_user_and_persists_history() {
    let mock_url = mock_anthropic_server(vec![END_TURN]).await;
    let app = test_app(&mock_url).await;

    // Send a message
    let resp = app
        .clone()
        .oneshot(post_json(
            "/message",
            serde_json::json!({
                "user_id": "test:history",
                "message": "Hello"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Check history
    let resp = app
        .oneshot(get("/history?user_id=test:history"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json_body(resp).await;
    let messages = body.as_array().expect("history should be an array");
    assert_eq!(messages.len(), 2); // user + assistant
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
}

#[tokio::test]
async fn test_history_empty_for_unknown_user() {
    let app = test_app("http://unused").await;

    let resp = app
        .oneshot(get("/history?user_id=nonexistent"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json_body(resp).await;
    let messages = body.as_array().expect("history should be an array");
    assert!(messages.is_empty());
}

#[tokio::test]
async fn test_skills_empty_for_new_user() {
    let app = test_app("http://unused").await;

    let resp = app.oneshot(get("/skills?user_id=test:1")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json_body(resp).await;
    let skills = body.as_array().expect("skills should be an array");
    assert!(skills.is_empty());
}

#[tokio::test]
async fn test_add_skill_and_list() {
    let skill_url = mock_skill_server().await;
    let app = test_app("http://unused").await;

    // Add skill
    let resp = app
        .clone()
        .oneshot(post_json(
            "/skills/add",
            serde_json::json!({
                "user_id": "test:skill",
                "name": "test-skill",
                "url": skill_url
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json_body(resp).await;
    assert_eq!(body["skill"], "test-skill");
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0], "test-skill__greet");

    // List skills
    let resp = app
        .oneshot(get("/skills?user_id=test:skill"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json_body(resp).await;
    let skills = body.as_array().unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["name"], "test-skill");
}

#[tokio::test]
async fn test_approvals_empty() {
    let app = test_app("http://unused").await;

    let resp = app.oneshot(get("/approvals?user_id=test:1")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json_body(resp).await;
    let approvals = body.as_array().unwrap();
    assert!(approvals.is_empty());
}

#[tokio::test]
async fn test_message_tool_use_then_end_turn() {
    // Mock returns tool_use on first call, then end_turn on second.
    // The tool "web_search" has no registered skill, so tool execution errors,
    // but the agent loop resumes and the LLM returns end_turn.
    let mock_url = mock_anthropic_server(vec![TOOL_USE, END_TURN]).await;
    let app = test_app(&mock_url).await;

    let resp = app
        .oneshot(post_json(
            "/message",
            serde_json::json!({
                "user_id": "test:tools",
                "message": "Search for something"
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    // After tool error + resume, the LLM returns end_turn with an answer
    assert_eq!(body["answer"], "Hello! How can I help you today?");
}

#[tokio::test]
async fn test_approve_nonexistent_returns_error() {
    let app = test_app("http://unused").await;

    let resp = app
        .oneshot(post_json(
            "/approve",
            serde_json::json!({
                "user_id": "test:1",
                "id": 999,
                "approve": true
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = json_body(resp).await;
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_multi_turn_conversation() {
    let mock_url = mock_anthropic_server(vec![END_TURN, END_TURN]).await;
    let app = test_app(&mock_url).await;

    // First message
    let resp = app
        .clone()
        .oneshot(post_json(
            "/message",
            serde_json::json!({
                "user_id": "test:multi",
                "message": "First message"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Second message
    let resp = app
        .clone()
        .oneshot(post_json(
            "/message",
            serde_json::json!({
                "user_id": "test:multi",
                "message": "Second message"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // History should have 4 messages (2 user + 2 assistant)
    let resp = app
        .oneshot(get("/history?user_id=test:multi"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = json_body(resp).await;
    let messages = body.as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[3]["role"], "assistant");
}

// --- Scheduled task tests ---

#[tokio::test]
async fn test_schedule_one_shot_task() {
    let mock_url = mock_anthropic_server(vec![END_TURN]).await;
    let (state, app) = test_app_with_state(&mock_url).await;

    // Create a one-shot task
    let resp = app
        .clone()
        .oneshot(post_json(
            "/tasks/schedule",
            serde_json::json!({
                "user_id": "test:sched",
                "name": "one-shot-test",
                "run_at": 1000,
                "payload": { "message": "Hello from scheduled task" }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["name"], "one-shot-test");
    assert_eq!(body["next_run_at"], 1000);
    let task_id = body["id"].as_i64().unwrap();

    // Verify it appears in GET /tasks
    let resp = app
        .clone()
        .oneshot(get("/tasks?user_id=test:sched"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let tasks = body.as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["name"], "one-shot-test");

    // run_at is in the past (1000ms epoch), so poll_once should fire it
    let scheduler = Scheduler::new(state.db.clone(), state.config.clone());
    scheduler.poll_once().await.unwrap();

    // After poll_once, the task is disabled immediately (before spawn)
    let resp = app.oneshot(get("/tasks?user_id=test:sched")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let tasks = body.as_array().unwrap();
    assert!(
        tasks.is_empty(),
        "one-shot task should be disabled after execution"
    );

    // Wait for the spawned task to finish and set last_run
    let last_run = wait_for_last_run(&state.db, task_id).await;
    assert!(last_run > 0, "last_run should be set after execution");
}

#[tokio::test]
async fn test_schedule_cron_task_rearms() {
    // Mock returns END_TURN for each agent turn triggered by the scheduler
    let mock_url = mock_anthropic_server(vec![END_TURN, END_TURN, END_TURN]).await;
    let (state, app) = test_app_with_state(&mock_url).await;

    // Ensure user exists
    edgeclaw_server::agent::ensure_user(&state.db, "test:cron")
        .await
        .unwrap();

    // Insert a cron task directly with run_at in the past so it fires immediately.
    // "* * * * * * *" = every second (7-field cron with seconds).
    sqlx::query(
        "INSERT INTO scheduled_tasks (user_id, name, cron, run_at, payload) VALUES (?, ?, ?, ?, ?)",
    )
    .bind("test:cron")
    .bind("cron-test")
    .bind("* * * * * * *")
    .bind(1000_i64) // far in the past
    .bind(r#"{"message":"cron tick"}"#)
    .execute(&state.db)
    .await
    .unwrap();

    // Get task id for wait helper
    let (task_id,): (i64,) =
        sqlx::query_as("SELECT id FROM scheduled_tasks WHERE name = 'cron-test'")
            .fetch_one(&state.db)
            .await
            .unwrap();

    let scheduler = Scheduler::new(state.db.clone(), state.config.clone());

    // First poll: re-arms run_at synchronously, spawns execution
    scheduler.poll_once().await.unwrap();

    // run_at is re-armed immediately (before spawn)
    let (run_at_1,): (Option<i64>,) =
        sqlx::query_as("SELECT run_at FROM scheduled_tasks WHERE name = 'cron-test'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    let run_at_1 = run_at_1.unwrap();
    assert!(
        run_at_1 > 1000,
        "run_at should advance past the original value"
    );

    // Wait for spawned task to set last_run
    let last_run_1 = wait_for_last_run(&state.db, task_id).await;

    // Set run_at back to the past so we can fire again without waiting.
    // Also clear last_run so we can detect the second write.
    sqlx::query(
        "UPDATE scheduled_tasks SET run_at = 1000, last_run = NULL WHERE name = 'cron-test'",
    )
    .execute(&state.db)
    .await
    .unwrap();

    // Second poll: fires again and re-arms
    scheduler.poll_once().await.unwrap();

    let (run_at_2,): (Option<i64>,) =
        sqlx::query_as("SELECT run_at FROM scheduled_tasks WHERE name = 'cron-test'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert!(
        run_at_2.unwrap() > 1000,
        "run_at should re-arm after second fire"
    );

    // Wait for second execution
    let last_run_2 = wait_for_last_run(&state.db, task_id).await;
    assert!(last_run_2 >= last_run_1, "last_run should advance");

    // Task should still be enabled (it's recurring)
    let resp = app.oneshot(get("/tasks?user_id=test:cron")).await.unwrap();
    let body = json_body(resp).await;
    let tasks = body.as_array().unwrap();
    assert_eq!(tasks.len(), 1, "cron task should still be enabled");
}

#[tokio::test]
async fn test_schedule_invalid_cron_rejected() {
    let app = test_app("http://unused").await;

    let resp = app
        .oneshot(post_json(
            "/tasks/schedule",
            serde_json::json!({
                "user_id": "test:bad-cron",
                "name": "bad-cron",
                "cron": "not a valid cron",
                "payload": { "message": "nope" }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert!(body["error"].as_str().unwrap().contains("invalid cron"));
}

#[tokio::test]
async fn test_list_and_delete_tasks() {
    let app = test_app("http://unused").await;

    // Create a one-shot task
    let resp = app
        .clone()
        .oneshot(post_json(
            "/tasks/schedule",
            serde_json::json!({
                "user_id": "test:del",
                "name": "deletable",
                "run_at": 9999999999999_i64,
                "payload": { "message": "will be deleted" }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let task_id = body["id"].as_i64().unwrap();

    // List — should see it
    let resp = app
        .clone()
        .oneshot(get("/tasks?user_id=test:del"))
        .await
        .unwrap();
    let body = json_body(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);

    // Delete
    let resp = app
        .clone()
        .oneshot(delete_req(&format!("/tasks/{task_id}?user_id=test:del")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["deleted"], true);

    // List — should be empty
    let resp = app.oneshot(get("/tasks?user_id=test:del")).await.unwrap();
    let body = json_body(resp).await;
    assert!(body.as_array().unwrap().is_empty());
}
