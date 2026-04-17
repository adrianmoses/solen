# Architecture

<!-- status: inferred -->
| Field | Value |
|---|---|
| status | inferred |
| created | 2026-04-17 |
| inferred-from | `Cargo.toml`, `crates/agent-core/src/{lib,agent,types,llm,soul,permissions,tools,builtins/*}.rs`, `crates/edgeclaw-server/src/{server,session,agent,scheduler,oauth,handlers,startup,builtin_executor}.rs`, `crates/edgeclaw-server/migrations/0001_initial.sql`..`0005_souls.sql`, `crates/edgeclaw-cli/src/{main,chat,config,soul}.rs`, `skills/skill-{github,gmail,google-calendar}/SKILL.md`, `docker-compose.yml`, `docker/Dockerfile.server`, `deploy/Caddyfile`, `deploy/edgeclaw.service` |

## System Overview

EdgeClaw is a Rust workspace of six crates, deployed as Docker Compose on a VPS behind Caddy. Skills are external MCP servers reached over HTTP; the `skills/` directory holds only `SKILL.md` prompt-context files.

```
┌──────────────────────┐      ┌───────────────────────────────┐
│  edgeclaw-cli        │◀────▶│  edgeclaw-server (axum)       │
│  (terminal REPL /    │  WS  │  • HTTP router                │
│   config / soul)     │ HTTP │  • WebSocket sessions         │
└──────────────────────┘      │  • Scheduler (polls SQLite)   │
                              │  • OAuth + credential store   │
                              │  • SQLite persistence         │
                              └──────┬────────────────┬───────┘
                                     │                │
                         ┌───────────▼──────┐   ┌─────▼──────────────┐
                         │ agent-core       │   │ skill-registry     │
                         │ (ReAct loop,     │   │ (MCP tool routing) │
                         │  built-in tools, │   └─────┬──────────────┘
                         │  soul composer,  │         │
                         │  permissions)    │         │ JSON-RPC over HTTP
                         └──────┬───────────┘         │
                                │                     ▼
                                │ HTTP        ┌──────────────────────────┐
                                ▼             │ External MCP skills      │
                       ┌────────────────┐     │  workspace-mcp (Google:  │
                       │ Anthropic API  │     │    Gmail, Calendar,      │
                       │ (messages)     │     │    Drive)                │
                       └────────────────┘     │  GitHub MCP              │
                                              │  (user-registered URLs)  │
                                              └──────────────────────────┘
```

A single request flows: client (CLI or HTTP) → server → per-user `AgentContext` loaded from SQLite → `agent-core::Agent::run` iterates against the Anthropic API → each tool call dispatches either to a built-in executor or (namespaced `skill:tool`) to `skill-registry`, which HTTP-POSTs JSON-RPC to the relevant MCP server → results fed back into the loop until `StopReason::EndTurn`. Tools flagged by `PolicyChain` pause the loop via a `oneshot` channel and emit a `ConfirmationPrompt` to the WebSocket client; the user answers and the loop resumes.

## Component Map

### Workspace crates

| Crate | Role | Key files |
|---|---|---|
| `agent-core` | Pure Rust ReAct loop, domain types, built-in tools, permission policies, soul | `agent.rs`, `types.rs`, `llm.rs`, `permissions.rs`, `soul.rs`, `tools.rs`, `builtins/{bash,file_read,file_write,file_edit,glob_tool,grep_tool}.rs` |
| `mcp-client` | JSON-RPC 2.0 MCP protocol client (initialize, list_tools, call_tool) | `protocol.rs`, `client.rs` |
| `skill-registry` | Namespaced tool routing (`skill:tool`), implements `ToolExecutor` | `lib.rs` |
| `credential-store` | AES-256-GCM + HKDF encrypted credential storage, used by OAuth flow | per-crate `lib.rs` |
| `edgeclaw-server` | axum + sqlx host: HTTP, WebSocket, scheduler, OAuth, handlers | `server.rs`, `session.rs`, `agent.rs`, `scheduler.rs`, `oauth.rs`, `handlers.rs`, `startup.rs`, `builtin_executor.rs` |
| `edgeclaw-cli` | `edgeclaw` binary: `serve`, `chat`, `config`, `soul` | `main.rs`, `chat/`, `config/`, `soul.rs` |

### Skills

All skills are external MCP servers reached over HTTP. The `skills/` directory holds only `SKILL.md` prompt-context files — there is no in-repo skill implementation.

| Skill | Kind | Notes |
|---|---|---|
| `skills/skill-github` | `SKILL.md` only | GitHub MCP. Server connects to `SKILL_GITHUB_URL` (e.g. GitHub Copilot MCP) with `SKILL_GITHUB_AUTH_TOKEN`. |
| `skills/skill-gmail` | `SKILL.md` only | Gmail via the Google Workspace MCP server. |
| `skills/skill-google-calendar` | `SKILL.md` only | Google Calendar via the Google Workspace MCP server (`ghcr.io/taylorwilsdon/google_workspace_mcp`, see `docker-compose.yml`). |

`SKILL.md` bodies are loaded as system-prompt context when the matching skill is registered. See `crates/edgeclaw-server/src/startup.rs::load_skill_context`.

### Agent-core key types (`types.rs`)

- `Role` = `User | Assistant`
- `ContentBlock` = `Text | ToolUse { id, name, input } | ToolResult { tool_use_id, content, is_error } | CompactBoundary { summary }`
- `Message { role, content: Vec<ContentBlock>, created_at }`
- `ToolDefinition { name, description, input_schema }`
- `ToolCall { id, name, input }`
- `ToolResult` with helpers: `.error_for()`, `.ok()`, `.err()`, `.require_str()`
- `ToolExecutor` trait: `async fn execute(&self, ToolCall) -> Result<ToolResult>`; `is_concurrent_safe() -> bool` defaults to false.
- `AgentRunResult { new_messages, answer: Option<String>, pending_tool_calls }`
- `AgentContext` bundles messages + tools + system prompt fragments.

### Agent ReAct loop (`agent.rs`)

- `Agent<H: HttpBackend> { llm: LlmClient<H>, tool_executor: Option<Arc<dyn ToolExecutor>>, max_iterations, max_continuations }`
- `run(&self, ctx, user_message) -> AgentRunResult` — append user message, enter loop.
- `resume(&self, ctx, tool_results) -> AgentRunResult` — wrap results as a tool-result message, re-enter loop.
- Internal `agent_loop` iterates up to `max_iterations` (default 10), branches on `StopReason`:
  - `EndTurn` → return the assistant's answer.
  - `MaxTokens` → emit a "Continue from where you left off" user nudge (up to `max_continuations`, default 3).
  - `ToolUse` → extract `ToolCall`s, execute via `tool_executor` (or return pending tool calls if no executor is attached, so the server can handle approvals out-of-band), append results, loop.
- `HttpBackend` trait abstracts the HTTP layer (per `CLAUDE.md`): `ReqwestBackend` for native, `MockHttpBackend` for tests (see `tests/fixtures/`).

### Permission chain (`permissions.rs`)

`PolicyChain` evaluates in strict order and returns the **first** `Some(PermissionCheck)`:

1. **DenyListPolicy** — hard-blocks shell invariants like `rm -rf /`, `mkfs`, `dd if=`, fork-bomb patterns.
2. **AllowListPolicy** — auto-approves read-only tools: `file_read`, `glob`, `grep`, `memory_fetch`, `memory_list`. Strips the MCP namespace prefix (`skill:tool` → `tool`) before matching.
3. **DestructivePatternPolicy** — requires approval for tools whose names contain `delete`, `remove`, `send`, or `drop`, plus an explicit list (`create_pull_request`, `merge_pull_request`, `issue_write`, …).
4. **DefaultRequiresApprovalPolicy** — catch-all: any tool not matched above requires approval.

### Soul (`soul.rs`)

- `Archetype` = `Assistant | Engineer | Researcher | Operator | Mentor`
- `Tone` = `Neutral | Friendly | Direct | Formal`
- `Verbosity` = `Terse | Balanced | Thorough`
- `DecisionStyle` = `Cautious | Balanced | Autonomous`
- `Soul { name, personality, archetype, tone, verbosity, decision_style }`
- `compose_system_prompt(&Soul) -> String` — concatenates identity header + archetype fragment + personality text + trait lines.
- `parse_soul_md` / `to_soul_md` — round-trip YAML-frontmatter + markdown body.

### Server (`edgeclaw-server`)

**HTTP routes** (assembled in `server.rs`):

| Method | Path | Purpose |
|---|---|---|
| GET | `/health` | Liveness check. |
| POST | `/message` | Single-turn HTTP entry (AutoApprove — no prompts). |
| GET | `/history?user_id=` | Fetch conversation history. |
| DELETE | `/history?user_id=` | Clear history. |
| POST | `/skills/add` | Register an MCP skill URL for the user. |
| GET | `/skills` | List registered skills. |
| DELETE | `/skills/{name}` | Remove a skill. |
| POST | `/approve` | Approve/deny a pending tool call. |
| GET | `/approvals` | List pending approvals. |
| POST | `/tasks/schedule` | Create a cron or one-shot scheduled task. |
| GET | `/tasks` | List scheduled tasks. |
| DELETE | `/tasks/{id}` | Delete a scheduled task. |
| POST | `/oauth/start` | Initiate OAuth with PKCE for a skill. |
| GET | `/oauth/callback` | Provider callback; exchanges the code and stores encrypted tokens. |
| POST | `/credentials/import-service-account` | Import a service-account credential. |
| GET/POST/PATCH | `/soul` | Read / create / partially update the agent's soul. |
| POST | `/soul/generate` | LLM-assisted soul generation from a description. |
| GET | `/ws` | WebSocket upgrade (primary bidirectional channel). |
| GET | `/admin/skills/status` | MCP health check across registered skills. |

**WebSocket session (`session.rs`)**:

- `ServerMessage` = `SessionStarted | AgentResponse { answer } | ConfirmationPrompt { request_id, tool_calls, reasons } | ToolExecuted { tool_name, success } | AgentError`.
- `ClientMessage` = `UserMessage { message } | ApprovalResponse { request_id, approved }`.
- `SessionHandle { server_tx: mpsc::Sender, user_id, pending_approvals: Arc<Mutex<HashMap<RequestId, oneshot::Sender<bool>>>> }`.
- Approval flow: agent emits `ConfirmationPrompt`, parks on the `oneshot::Receiver`; client replies with `ApprovalResponse`; the server sends the bool into the oneshot and the loop resumes. Unanswered prompts auto-deny after 5 minutes (README §Tool approval flow).

**Scheduler (`scheduler.rs`)**:

- Polls `scheduled_tasks` every 10 seconds for rows with `enabled=1` and `run_at <= now`.
- One-shot tasks: fire once, then `enabled=0`.
- Cron tasks: parse with `cron 0.15`, compute `next run_at` *before* spawning the turn so the next tick is deterministic, then leave `enabled=1`.
- Task execution is `tokio::spawn`'d and calls `edgeclaw_server::agent::run_agent_turn` with `AutoApprove` (scheduled runs don't prompt — no human is there).
- Invalid cron expressions disable the task and emit a tracing error.

**OAuth (`oauth.rs`)**:

- PKCE primitives: `generate_nonce`, `generate_code_verifier`, `compute_code_challenge`.
- `ProviderConfig` (client_id/secret, auth_url, token_url, default_scopes, extra_auth_params) — GitHub and Google are loaded from env (`GITHUB_OAUTH_*`, `GOOGLE_OAUTH_*`). [INFERRED: uncertain — exact env names not re-verified.]
- `OAuthFlowState` kept in memory with an `expires_at` so callbacks can only succeed within the flow window.
- Resulting tokens are encrypted (AES-256-GCM + HKDF, see `SKILL_ENCRYPTION_KEY`) and stored in `credentials`.

### CLI (`edgeclaw-cli`)

- `main.rs` dispatches clap derive subcommands:
  - `serve [--host] [--port]` — start the axum server in-process.
  - `chat [--connect ws://...]` — inline raw-mode TUI; attempts WS connect, spawns a local server in-process if one isn't running and tears it down on exit. Tool approvals rendered inline as `y/n`.
  - `config [show|edit|set ...|connector ...]` — TOML at `~/.config/edgeclaw/config.toml` (or `$EDGECLAW_CONFIG`, or `--config`). Atomic writes, env-var overrides.
  - `soul [show|set|edit|generate|import|export]` — REST wrappers around `/soul*` endpoints; `SOUL.md` I/O.

### SQLite schema (`crates/edgeclaw-server/migrations/`)

Migrations applied in order via `sqlx migrate`:

- **`0001_initial.sql`**:
  - `users (id TEXT PK, created_at INT)`
  - `messages (id INTEGER AI, user_id FK, role TEXT, content TEXT JSON, created_at INT)` + index `(user_id, created_at DESC)`
  - `skills (user_id, name PK, url, tools JSON, added_at)`
  - `credentials (user_id, skill_name, provider PK, access_token_enc, refresh_token_enc, expires_at, scopes, user_salt, created_at, updated_at)`
  - `scheduled_tasks (id AI, user_id FK, name, cron, run_at, payload, last_run, enabled)` + partial index `(run_at) WHERE run_at IS NOT NULL AND enabled=1`
  - `pending_approvals (id AI, user_id FK, tool_call JSON, created_at)`
  - `prefs (user_id, key PK, value)`
  - `memory_facts (id AI, user_id FK, key UNIQUE, value, tags, created_at)`
- **`0002_credential_type.sql`** — adds credential type discrimination (OAuth vs service-account).
- **`0003_skill_context.sql`** — adds per-skill context.
- **`0004_skill_auth_headers.sql`** — adds auth-header metadata to skills.
- **`0005_souls.sql`** — adds per-user souls table.

## Data Flow

### Interactive WebSocket turn

1. Client connects to `GET /ws`, sends handshake `{ "user_id": "default" }`.
2. Server creates `SessionHandle`, emits `SessionStarted`, stores `pending_approvals` map.
3. Client sends `{ "type": "user_message", "message": "..." }`.
4. Server loads `AgentContext` (history from `messages`, registered skills and their tools, soul from `souls`, built-in tool definitions).
5. `Agent::run` posts to the Anthropic Messages API via `ReqwestBackend`.
6. On `ToolUse` stop reason: the server runs each tool call through `PolicyChain`. If policy returns `RequiresApproval`, the server emits `ConfirmationPrompt` and waits on the `oneshot::Receiver`; otherwise it dispatches:
   - Built-in → `builtin_executor` → local FS/shell.
   - `skill:tool` → `skill-registry` → HTTP POST JSON-RPC 2.0 to the skill's URL with auth headers (and decrypted credentials if configured).
7. Tool results append to `ContentBlock::ToolResult` and the loop iterates.
8. On `EndTurn`: assistant message is persisted to `messages`, `AgentResponse { answer }` is sent over WS.

### HTTP `POST /message`

Same path, but policy evaluation is short-circuited by `AutoApprove`: every tool call runs without a prompt. No `ConfirmationPrompt` is emitted.

### Scheduled task

1. Scheduler tick (every 10s) picks up due rows.
2. Spawns a tokio task that calls `run_agent_turn(user_id, payload, AutoApprove)`.
3. Writes assistant reply into `messages`, updates `last_run`, recomputes `run_at` (for cron) or sets `enabled=0` (one-shot).

### OAuth credential enrollment

1. Client calls `POST /oauth/start { provider, skill_name, user_id }`.
2. Server creates `OAuthFlowState` with PKCE verifier, returns the provider auth URL.
3. User authenticates with provider → redirected to `GET /oauth/callback?code=...&state=...`.
4. Server exchanges code + verifier for tokens, encrypts with AES-256-GCM + HKDF (derived per user), writes to `credentials`.
5. Subsequent `skill-registry` calls decrypt and attach `Authorization` headers to MCP requests.

## External Dependencies

**Runtime services**
- **Anthropic Messages API** (`ANTHROPIC_BASE_URL`, default `https://api.anthropic.com`) — required, gated on `ANTHROPIC_API_KEY`.
- **Google Workspace MCP** (`ghcr.io/taylorwilsdon/google_workspace_mcp` in `docker-compose.yml`) — provides Gmail / Calendar / Drive tools in lieu of first-party in-repo skills.
- **GitHub / Google OAuth** — `*_OAUTH_CLIENT_ID` and `*_OAUTH_CLIENT_SECRET` env vars; PKCE flow.

**Infrastructure**
- **Caddy 2-alpine** as the reverse proxy (`deploy/Caddyfile` — `reverse_proxy agent:8080`, listens on `${DOMAIN}:80`).
- **Docker Compose** for orchestration (dev: `docker-compose.yml`, prod override: `docker-compose.prod.yml` pulls `ghcr.io/adrianmoses/edgeclaw:latest`).
- **systemd oneshot** (`deploy/edgeclaw.service`) wraps compose on the host, working directory `/opt/edgeclaw`, 120s start timeout.

**Rust crate dependencies** (top of each `Cargo.toml`)
- Server: `axum 0.8`, `tokio 1`, `sqlx 0.8` (sqlite+migrate), `reqwest 0.12`, `tower-http 0.6`, `tracing 0.1`, `jsonwebtoken 9`, `cron 0.15`, `chrono 0.4`, `base64 0.22`, `sha2 0.10`, `rand 0.8`, `url 2`.
- Agent-core: `serde`, `serde_json`, `serde_yaml`, `async-trait`, `thiserror`; native feature adds `reqwest`, `tokio`, `glob`, `regex`, `walkdir`.
- CLI: `clap 4`, `tokio-tungstenite 0.28`, `crossterm 0.28`, `inquire 0.7`, `toml 0.8`, `dirs 6`.

## Key Constraints

- **Single-tenant trust model.** No authentication on HTTP or WebSocket. `user_id` is supplied by the client and is purely namespace, not identity. Safe only behind a private reverse proxy or on localhost.
- **SQLite = single-writer.** No horizontal scale-out; concurrency assumes one server process.
- **`agent-core` has zero server-framework dependency** (hard rule in `CLAUDE.md`). `HttpBackend` trait is the seam so the agent loop can be mock-tested without bringing in axum/reqwest.
- **`MAX_TASKS_PER_USER`** (default 20) env var caps scheduled tasks per user. [INFERRED: uncertain — confirm the scheduler/handler enforces it.]
- **5-minute approval timeout** on WebSocket confirmation prompts. After that, the tool is auto-denied and an error is fed back to the LLM.
- **Max 10 ReAct iterations per turn** (`max_iterations`) and max 3 `MaxTokens` continuations (`max_continuations`). These are configurable on `Agent` but currently hardcoded at construction.
- **Permission `PolicyChain` order is load-bearing.** Re-ordering layers would silently change which tools need approval; keep deny before allow before destructive-pattern before default.
- **Credential encryption requires `SKILL_ENCRYPTION_KEY`.** AES-256-GCM with HKDF-per-user-salt; losing the key unrecoverably invalidates stored tokens.
- **Default ports differ between the server binary and the CLI.** Direct `cargo run -p edgeclaw-server` binds `0.0.0.0:8080`; `edgeclaw serve` defaults to `127.0.0.1:7100`. A client reaching the wrong port would silently fall through to "spawn local server" behavior.
- **Messages stored as JSON `TEXT`** in SQLite. No relational querying of content blocks; changes to `ContentBlock` serde schema are an implicit migration risk.
