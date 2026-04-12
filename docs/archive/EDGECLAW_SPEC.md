# EdgeClaw — Architecture Specification

> A stateful, self-hosted personal AI agent runtime built on tokio + axum + sqlx,
> deployed via Docker Compose on a Hetzner VPS.
>
> _This spec supersedes the original Cloudflare Workers/Durable Objects spec (archived at `docs/archive/EDGECLAW_SPEC_CF.md`).
> Companion specs: [Credentials](EDGECLAW_CREDENTIALS_SPEC.md), [TUI](EDGECLAW_TUI_SPEC.md)._

---

## What Changes, What Doesn't

The migration is entirely a runtime-layer swap. The domain logic is untouched.

| Crate | Status | Notes |
|---|---|---|
| `agent-core` | **Unchanged** | Pure Rust, no CF dependency |
| `mcp-client` | **Unchanged** | Pure Rust HTTP client |
| `skill-registry` | **Unchanged** | Pure Rust |
| `credential-store` | **Unchanged** | `aes-gcm` + `ring`, no WASM constraints now |
| `edgeclaw-worker` | **Deleted** | Was the workers-rs DO host — gone |
| `edgeclaw-server` | **New** | tokio + axum + sqlx — replaces edgeclaw-worker |
| `edgeclaw-cli` | **Minor changes** | `wrangler deploy` → SSH + Docker |
| `skill-*` Workers | **Unchanged** | Still stateless HTTP servers, just deployed differently |

The MCP skill architecture is protocol-level and survives intact. Skills remain isolated HTTP services. The credential encryption scheme (`aes-gcm` + HKDF via `ring`) is now unconstrained — use either freely.

---

## Repository Structure After Migration

```
edgeclaw/
├── crates/
│   ├── agent-core/          # Unchanged
│   ├── mcp-client/          # Unchanged
│   ├── skill-registry/      # Unchanged
│   ├── credential-store/    # Unchanged (ring now unrestricted)
│   ├── edgeclaw-server/     # NEW — replaces edgeclaw-worker
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   ├── server.rs        # axum router, AppState
│   │   │   ├── agent.rs         # per-user agent task management
│   │   │   ├── scheduler.rs     # tokio-based task scheduling
│   │   │   ├── db.rs            # sqlx pool, migrations
│   │   │   └── handlers/        # axum route handlers
│   │   ├── migrations/          # sqlx migration files
│   │   └── Cargo.toml
│   └── edgeclaw-cli/        # Minor changes to deploy flow
├── skills/
│   ├── skill-memory/        # Now a tokio process, not a DO
│   ├── skill-web-search/    # Unchanged logic, different deploy
│   ├── skill-http-fetch/    # Unchanged logic, different deploy
│   └── skill-gmail/         # etc.
├── docker/
│   ├── docker-compose.yml
│   ├── docker-compose.dev.yml
│   ├── Dockerfile.server
│   └── Dockerfile.skill     # shared base for skill services
├── deploy/
│   └── edgeclaw.service     # systemd unit
└── docs/
```

---

## Part 1 — `edgeclaw-server` (replaces `edgeclaw-worker`)

### 1.1 — Primitive Mapping

Every Cloudflare primitive has a direct, simpler equivalent:

| Cloudflare (before) | VPS equivalent (after) |
|---|---|
| `DurableObject` per user | `AgentState` per user in SQLite |
| `State::storage().sql()` | `sqlx::SqlitePool` |
| `ObjectNamespace::id_from_name()` | User ID string as SQLite row key |
| `Stub::fetch()` (DO-to-DO) | `reqwest` HTTP calls between services |
| DO alarm | `tokio::time::sleep` + persisted task table |
| `#[event(fetch)]` dispatcher | `axum::Router` |
| Worker secret bindings | Environment variables from `.env` |
| WebSocket hibernation | `tokio-tungstenite` persistent connection |
| Cron Trigger | `tokio::time::interval` background task |
| Workers KV | Not needed — SQLite covers it |

### 1.2 — Application State

The `AppState` struct is shared across all axum handlers via `Arc`. It holds the database pool and a handle to the scheduler. No per-request state reconstruction needed — the pool is always open.

```rust
// crates/edgeclaw-server/src/server.rs

pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<ServerConfig>,
    pub scheduler: Arc<Scheduler>,
}

pub struct ServerConfig {
    pub anthropic_api_key: String,
    pub token_master_key: [u8; 32],   // decoded from env at startup
    pub telegram_bot_token: Option<String>,
    pub telegram_allowed_user_id: Option<i64>,
    pub default_model: String,
    pub max_iterations: u8,
}
```

`Arc<AppState>` is passed to all axum handlers via `.with_state()`. No `RwLock` needed — `SqlitePool` is already `Clone + Send + Sync`.

### 1.3 — SQLite Schema

The schema is nearly identical to what was in the DO. The one addition is a `users` table — the DO used its identity as the user boundary; here it's an explicit row.

Migrations live in `crates/edgeclaw-server/migrations/` as numbered `.sql` files. `sqlx::migrate!()` embeds and runs them at startup.

```sql
-- migrations/0001_initial.sql

CREATE TABLE IF NOT EXISTS users (
    id          TEXT PRIMARY KEY,   -- e.g. "telegram:123456789"
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT    NOT NULL REFERENCES users(id),
    role        TEXT    NOT NULL,   -- "user" | "assistant"
    content     TEXT    NOT NULL,   -- JSON Vec<ContentBlock>
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS skills (
    user_id     TEXT    NOT NULL REFERENCES users(id),
    name        TEXT    NOT NULL,
    url         TEXT    NOT NULL,
    tools       TEXT    NOT NULL,   -- JSON Vec<ToolDefinition>
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (user_id, name)
);

CREATE TABLE IF NOT EXISTS credentials (
    user_id           TEXT    NOT NULL REFERENCES users(id),
    skill_name        TEXT    NOT NULL,
    provider          TEXT    NOT NULL,
    access_token_enc  BLOB    NOT NULL,
    refresh_token_enc BLOB,
    expires_at        INTEGER,
    scopes            TEXT    NOT NULL,
    user_salt         BLOB    NOT NULL,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    PRIMARY KEY (user_id, skill_name, provider)
);

CREATE TABLE IF NOT EXISTS scheduled_tasks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT    NOT NULL REFERENCES users(id),
    name        TEXT    NOT NULL,
    cron        TEXT,               -- cron expression, nullable
    run_at      INTEGER,            -- unix ms for one-shot tasks, nullable
    payload     TEXT    NOT NULL,   -- JSON task params
    last_run    INTEGER,
    enabled     INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS pending_approvals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT    NOT NULL REFERENCES users(id),
    tool_call   TEXT    NOT NULL,   -- JSON ToolCall
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS prefs (
    user_id     TEXT    NOT NULL REFERENCES users(id),
    key         TEXT    NOT NULL,
    value       TEXT    NOT NULL,
    PRIMARY KEY (user_id, key)
);

CREATE INDEX IF NOT EXISTS idx_messages_user_created
    ON messages(user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_tasks_run_at
    ON scheduled_tasks(run_at) WHERE run_at IS NOT NULL AND enabled = 1;
```

### 1.4 — Agent Turn Execution

Where the DO had a `fetch()` handler that owned the turn loop, `edgeclaw-server` has an axum handler that calls into `agent-core`. The logic is identical — the only difference is where state comes from (SQLite via sqlx instead of DO storage).

The turn function is a free async function, not a method on a DO. It takes the pool, the user ID, and the incoming message:

```rust
// crates/edgeclaw-server/src/agent.rs (prose description, not full code)
```

**Turn execution steps:**

1. Upsert the user row (create if first message)
2. Load conversation history — `SELECT ... WHERE user_id = ? ORDER BY created_at DESC LIMIT 50`
3. Load registered skills from the `skills` table, reconnect MCP clients
4. Load system prompt from `prefs`
5. Assemble `AgentContext` and call `agent_core::Agent::run(ctx, message)`
6. Persist `new_messages` in a single `INSERT` transaction
7. If `pending_tool_calls` — check for destructive calls, dispatch or pause
8. Re-execute from step 2 using `agent_core::Agent::resume()` until `answer` is set
9. Return the final answer string

Critically, steps 6 and 7 happen inside a **sqlx transaction** — if the process crashes mid-turn, the partially completed turn is rolled back. On restart the user's last message is unanswered but no corrupted half-state exists in the DB.

### 1.5 — Scheduler (replaces DO Alarms)

The DO alarm API provided per-instance timers that survived eviction. On the VPS, this is a background tokio task that polls the `scheduled_tasks` table on startup and wakes tasks at their scheduled time.

```rust
// crates/edgeclaw-server/src/scheduler.rs (prose description)
```

**Scheduler design:**

- On startup, spawns a `tokio::task` that runs a polling loop
- Every 10 seconds, queries `scheduled_tasks` for tasks where `run_at <= now() AND enabled = 1`
- For cron tasks, uses the `cron` crate to compute the next `run_at` after each execution and updates the row
- For one-shot tasks, sets `enabled = 0` after execution
- Each task execution calls back into the agent turn loop with a system-generated message (e.g. `"[SCHEDULED] Run morning briefing"`)
- Tasks persist across restarts — they're in SQLite, not in memory

This handles the use cases that broke on Cloudflare: "send me a daily briefing at 8am", "check for new GitHub PRs every 30 minutes". The tokio process is always running; there is no eviction.

### 1.6 — axum Router

```
POST /message               — inbound message from Telegram or direct HTTP client
GET  /ws                    — WebSocket upgrade for streaming responses
POST /oauth/callback        — OAuth redirect handler (was OAuthCallbackWorker)
POST /skills/add            — register a new MCP skill URL
GET  /skills                — list registered skills for a user
POST /credentials/store     — store encrypted credential (called internally)
GET  /admin/status          — health + stats, used by ratatui management TUI
GET  /manage                — serves the ratzilla WASM bundle (static files)
```

Authentication on all routes: `Authorization: Bearer {ADMIN_TOKEN}` for the admin routes, Telegram's webhook secret header for `/message`, user session token for `/ws` and `/manage`.

### 1.7 — `main.rs` Structure

```rust
// crates/edgeclaw-server/src/main.rs (prose)
```

Startup sequence:

1. Load config from environment variables (via `dotenvy` in dev, Docker env in prod)
2. Open SQLite pool: `SqlitePoolOptions::new().max_connections(1).connect(&db_url)`  
   — SQLite is single-writer; max_connections(1) on the write pool prevents contention
3. Run `sqlx::migrate!()` — applies any pending migrations
4. Spawn the scheduler background task
5. Spawn the Telegram polling task (or register webhook) if configured
6. Start the axum server on `0.0.0.0:8080`
7. Register `SIGTERM` / `SIGINT` handlers for graceful shutdown — drain in-flight requests, close pool

**SQLite connection note:** Use `max_connections(1)` for the write pool and a separate read pool with higher concurrency. SQLite allows multiple concurrent readers but only one writer. All writes from the agent turn loop, scheduler, and credential store go through the write pool, which serialises them automatically via the pool.

### 1.8 — Cargo.toml

```toml
# crates/edgeclaw-server/Cargo.toml
[dependencies]
agent-core       = { path = "../agent-core" }
mcp-client       = { path = "../mcp-client" }
skill-registry   = { path = "../skill-registry" }
credential-store = { path = "../credential-store" }

tokio            = { version = "1", features = ["full"] }
axum             = { version = "0.8", features = ["ws"] }
tower-http       = { version = "0.6", features = ["fs", "cors", "trace"] }
sqlx             = { version = "0.8", features = ["runtime-tokio", "sqlite", "macros", "migrate"] }
reqwest          = { version = "0.12", features = ["json"] }
serde            = { version = "1", features = ["derive"] }
serde_json       = "1"
tokio-tungstenite = "0.24"
cron             = "0.12"
dotenvy          = "0.15"
tracing          = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow           = "1"
thiserror        = "1"
zeroize          = "1"
rand             = { version = "0.8", features = ["getrandom"] }
base64           = "0.22"

# ring and aes-gcm — no WASM constraints here, use freely
ring             = "0.17"
aes-gcm          = "0.10"
```

---

## Part 2 — Docker Compose

### 2.1 — Service Layout

The compose file runs three categories of service:

- **`agent`** — the main `edgeclaw-server` process
- **`skill-*`** — one container per skill Worker (stateless HTTP)
- **`caddy`** — reverse proxy handling TLS termination and routing

```
internet
    │  HTTPS
    ▼
┌─────────┐
│  Caddy  │  TLS termination, routing
└────┬────┘
     │
     ├──────────────────────────────┐
     ▼                              ▼
┌──────────────┐          ┌─────────────────┐
│   agent      │          │  skill services │
│  (port 8080) │          │  (8081, 8082,…) │
│              │          │                 │
│  SQLite vol  │          │  stateless      │
└──────────────┘          └─────────────────┘
```

### 2.2 — `docker-compose.yml`

```yaml
name: edgeclaw

services:

  agent:
    build:
      context: .
      dockerfile: docker/Dockerfile.server
    restart: unless-stopped
    env_file: .env
    volumes:
      - agent-data:/data          # SQLite lives here
    ports:
      - "127.0.0.1:8080:8080"    # only exposed to localhost; Caddy proxies
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 30s
      timeout: 5s
      retries: 3
    depends_on:
      - skill-web-search
      - skill-http-fetch

  skill-web-search:
    build:
      context: .
      dockerfile: docker/Dockerfile.skill
      args:
        SKILL: skill-web-search
    restart: unless-stopped
    env_file: .env.skills
    ports:
      - "127.0.0.1:8081:8080"

  skill-http-fetch:
    build:
      context: .
      dockerfile: docker/Dockerfile.skill
      args:
        SKILL: skill-http-fetch
    restart: unless-stopped
    ports:
      - "127.0.0.1:8082:8080"

  skill-gmail:
    build:
      context: .
      dockerfile: docker/Dockerfile.skill
      args:
        SKILL: skill-gmail
    restart: unless-stopped
    env_file: .env.skills
    ports:
      - "127.0.0.1:8083:8080"

  skill-github:
    build:
      context: .
      dockerfile: docker/Dockerfile.skill
      args:
        SKILL: skill-github
    restart: unless-stopped
    env_file: .env.skills
    ports:
      - "127.0.0.1:8084:8080"

  skill-google-calendar:
    build:
      context: .
      dockerfile: docker/Dockerfile.skill
      args:
        SKILL: skill-google-calendar
    restart: unless-stopped
    env_file: .env.skills
    ports:
      - "127.0.0.1:8085:8080"

  caddy:
    image: caddy:2-alpine
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./deploy/Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy-data:/data
      - caddy-config:/config

volumes:
  agent-data:
  caddy-data:
  caddy-config:
```

### 2.3 — `Dockerfile.server`

Uses `cargo-chef` to cache Rust dependency compilation across builds — without it, every source change rebuilds all dependencies from scratch, which is the main Docker+Rust pain point.

```dockerfile
# docker/Dockerfile.server

# Stage 1: cargo-chef planner — computes the dependency recipe
FROM rust:1.85-alpine AS chef
WORKDIR /app
RUN apk add --no-cache musl-dev sqlite-dev
RUN cargo install cargo-chef

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Stage 2: dependency cache — only rebuilt when Cargo.toml/Cargo.lock change
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Stage 3: actual build — fast when only src/ changed
COPY . .
RUN cargo build --release --bin edgeclaw-server

# Stage 4: minimal runtime image
FROM alpine:3.21 AS runtime
RUN apk add --no-cache ca-certificates sqlite-libs curl
RUN addgroup -S edgeclaw && adduser -S edgeclaw -G edgeclaw
COPY --from=builder /app/target/release/edgeclaw-server /usr/local/bin/
RUN mkdir -p /data && chown edgeclaw:edgeclaw /data
USER edgeclaw
VOLUME ["/data"]
ENV DATABASE_URL=sqlite:///data/edgeclaw.db
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s \
    CMD curl -f http://localhost:8080/health || exit 1
CMD ["edgeclaw-server"]
```

### 2.4 — `Dockerfile.skill`

All skill binaries share one Dockerfile parameterised by `ARG SKILL`. This avoids maintaining five nearly identical Dockerfiles.

```dockerfile
# docker/Dockerfile.skill

FROM rust:1.85-alpine AS chef
WORKDIR /app
RUN apk add --no-cache musl-dev
RUN cargo install cargo-chef

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
ARG SKILL
RUN cargo build --release --bin ${SKILL}

FROM alpine:3.21 AS runtime
RUN apk add --no-cache ca-certificates curl
RUN addgroup -S skill && adduser -S skill -G skill
ARG SKILL
COPY --from=builder /app/target/release/${SKILL} /usr/local/bin/skill-server
USER skill
EXPOSE 8080
CMD ["skill-server"]
```

### 2.5 — Environment Variables

Two env files — one for the agent, one for skills — so skill OAuth client secrets are not in the agent's environment.

```bash
# .env  (agent — never committed)
DATABASE_URL=sqlite:///data/edgeclaw.db
ANTHROPIC_API_KEY=sk-ant-...
TOKEN_MASTER_KEY=<32 random bytes, base64-encoded>
TELEGRAM_BOT_TOKEN=...
TELEGRAM_ALLOWED_USER_ID=123456789
AGENT_NAME=Aria
DEFAULT_MODEL=claude-sonnet-4-6
MAX_ITERATIONS=10
ADMIN_TOKEN=<random token for management API>

# Skill URLs — agent uses these to route MCP calls
SKILL_WEB_SEARCH_URL=http://skill-web-search:8080
SKILL_HTTP_FETCH_URL=http://skill-http-fetch:8080
SKILL_GMAIL_URL=http://skill-gmail:8080
SKILL_GITHUB_URL=http://skill-github:8080
SKILL_GOOGLE_CALENDAR_URL=http://skill-google-calendar:8080
```

```bash
# .env.skills  (skill secrets — never committed)
BRAVE_API_KEY=...
GITHUB_CLIENT_ID=...
GITHUB_CLIENT_SECRET=...
GOOGLE_CLIENT_ID=...
GOOGLE_CLIENT_SECRET=...
```

### 2.6 — Caddyfile

Caddy handles TLS automatically via Let's Encrypt. No cert management needed.

```
# deploy/Caddyfile

your-agent.domain.com {
    # Main agent API and WebSocket
    reverse_proxy /api/* agent:8080
    reverse_proxy /ws    agent:8080
    reverse_proxy /oauth/* agent:8080

    # ratzilla management TUI — served as static WASM
    reverse_proxy /manage* agent:8080

    # Telegram webhook
    reverse_proxy /telegram/* agent:8080
}
```

---

## Part 3 — systemd Service

Docker Compose itself runs under systemd. One unit file starts and stops the entire stack.

```ini
# deploy/edgeclaw.service
[Unit]
Description=EdgeClaw AI Agent
Requires=docker.service
After=docker.service network-online.target
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
WorkingDirectory=/opt/edgeclaw
ExecStart=/usr/bin/docker compose up -d --remove-orphans
ExecStop=/usr/bin/docker compose down
ExecReload=/usr/bin/docker compose pull && /usr/bin/docker compose up -d --remove-orphans
TimeoutStartSec=120
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

Deployment is then:

```bash
# First deploy
sudo cp deploy/edgeclaw.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable edgeclaw
sudo systemctl start edgeclaw

# Update (pulls new images and restarts changed containers only)
sudo systemctl reload edgeclaw

# View logs
sudo journalctl -u edgeclaw -f
# Or per-container:
docker compose logs -f agent
```

---

## Part 4 — CLI Changes (`edgeclaw setup`)

The wizard flow changes only in its final deployment step. Everything up to and including the pre-deployment summary is unchanged — the same `inquire` prompts, the same config collection.

**What replaces `wrangler deploy`:**

1. SSH to the VPS (credentials collected during setup, or via SSH key)
2. `rsync` the `.env` and `.env.skills` files to `/opt/edgeclaw/`
3. Pull the latest Docker images (`docker compose pull`)
4. Run `docker compose up -d --remove-orphans`
5. Wait for the health check to pass
6. Print the success message

The `edgeclaw-cli` uses the `ssh2` Rust crate for the SSH connection and the `openssh` crate for command execution. No dependency on `wrangler` or Node.js.

**New setup prompts (replacing the Cloudflare-specific ones):**

```
─── Server Setup ───────────────────────────────────────────
? VPS hostname or IP  ›  _______________
  (e.g. 123.456.789.0 or agent.yourdomain.com)

? SSH user  ›  root
  (The user Docker Compose will run under)

? SSH authentication
  › SSH key (recommended)
    Password

[If SSH key selected:]
? Path to private key  ›  ~/.ssh/id_ed25519
  ✓ Key found and readable

  ✓ Connecting to server...
  ✓ Docker found: 27.3.1
  ✓ Docker Compose found: 2.30.1
  ✓ /opt/edgeclaw directory created
```

The cost estimate in the summary screen updates:

```
  Estimated monthly cost:
    Hetzner CX22: ~€4.35/mo
    Anthropic API: ~$1–5/mo depending on usage
    Domain (optional): ~€1/mo
```

---

## Part 5 — What the `skill-*` Services Become

Skills were Cloudflare Workers — stateless HTTP handlers. On a VPS they are stateless tokio/axum services in Docker containers. The MCP protocol and tool definitions are unchanged. The only thing that changes is the runtime.

`skill-memory` is the exception — it was a Durable Object with its own SQLite. On the VPS it becomes a simple module inside `edgeclaw-server` itself, backed by the same SQLite database under the `memory_facts` table. There is no strong reason to keep it as a separate process when isolation is handled by the application layer rather than the platform.

```sql
-- Added to 0001_initial.sql or a new migration

CREATE TABLE IF NOT EXISTS memory_facts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT    NOT NULL REFERENCES users(id),
    key         TEXT    NOT NULL,
    value       TEXT    NOT NULL,
    tags        TEXT,               -- JSON array of strings
    created_at  INTEGER NOT NULL,
    UNIQUE(user_id, key)
);
```

The `skill-memory` MCP server becomes a set of axum route handlers on the main `edgeclaw-server` at `/skills/memory/*`, backed by this table. No separate container needed.

---

## Milestones

### Server Core (M5.1–M5.5)

| Milestone | Description | Done When |
|---|---|---|
| M5.1 | `edgeclaw-server` crate created, axum + sqlx wired up, health endpoint responds | `cargo run` starts server, `/health` returns 200 |
| M5.2 | SQLite migrations run on startup, all tables created | `sqlx::migrate!()` applies cleanly |
| M5.3 | Agent turn executes with real Anthropic API call, persists messages | Multi-turn conversation via `curl` works |
| M5.4 | Scheduler polls `scheduled_tasks` and fires one-shot tasks | One-shot task executes and marks itself done |
| M5.5 | Cron tasks compute next `run_at` and re-arm correctly | Recurring task fires at least twice |

### Credential Store — Envelope Encryption (M5.6)

_Detail: [Credentials Spec](EDGECLAW_CREDENTIALS_SPEC.md) Phase 1_

| Milestone | Description | Done When |
|---|---|---|
| M5.6.1 | `credential-store` crate compiles and unit tests pass | `cargo test -p credential-store` clean |
| M5.6.2 | HKDF derivation produces distinct keys for different providers and salts | Unit test with known vectors passes |
| M5.6.3 | AES-256-GCM round-trips plaintext through encrypt/decrypt | Property test: decrypt(encrypt(pt)) == pt for random pt |
| M5.6.4 | `store` and `load` persist and recover credentials via sqlx SQLite | Integration test with real SQLite round-trip |
| M5.6.5 | Tampered ciphertext or nonce causes decryption failure | Authentication tag rejection test passes |
| M5.6.6 | Token refresh detects expiry and calls provider token endpoint | Fixture-based refresh test with mock HTTP |
| M5.6.7 | Refresh failure marks credential invalid and surfaces error to user | Error propagation test passes |
| M5.6.8 | Plaintext buffers are zeroed on drop | `zeroize` integration confirmed in test |

### Credential Store — OAuth PKCE Flow (M5.7)

_Detail: [Credentials Spec](EDGECLAW_CREDENTIALS_SPEC.md) Phase 2_

| Milestone | Description | Done When |
|---|---|---|
| M5.7.1 | `OAuthFlowState` created in memory, generates PKCE pair | Unit test: code_challenge == BASE64URL(SHA256(code_verifier)) |
| M5.7.2 | `/oauth/callback` handler routes callback to correct flow by nonce | Integration test with test server |
| M5.7.3 | Token exchange completes against mock provider endpoint | Fixture-based token exchange test |
| M5.7.4 | Tokens written encrypted to SQLite via credential store | End-to-end: credential readable after flow |
| M5.7.5 | Cleanup task removes expired flows from in-memory map | Test: expired entry absent after cleanup runs |
| M5.7.6 | User notified via WebSocket on successful connection | WS message received after completion |
| M5.7.7 | Expired flow rejected cleanly | `complete` after `expires_at` returns error, no token written |
| M5.7.8 | Full PKCE round-trip against real GitHub OAuth sandbox | Manual end-to-end on `cargo run` or `docker compose up` |

### Credential Store — Skill Installation (M5.8)

_Detail: [Credentials Spec](EDGECLAW_CREDENTIALS_SPEC.md) Phase 3_

| Milestone | Description | Done When |
|---|---|---|
| M5.8.1 | `skill-gmail` container starts and lists messages with a real token | Manual smoke test against Gmail API |
| M5.8.2 | `skill-gmail` destructive tools trigger approval flow | Approval round-trip test via WebSocket |
| M5.8.3 | `skill-github` container starts and lists repos and issues | Manual smoke test against GitHub API |
| M5.8.4 | `skill-github` rate limit surfaced in tool error response | Rate limit header parsed and returned in ToolCallResult |
| M5.8.5 | `skill-google-calendar` container starts and lists events | Manual smoke test against Calendar API |
| M5.8.6 | `calendar_find_free_slots` returns correct free blocks | Unit test against known freebusy fixture |
| M5.8.7 | Token refresh works end-to-end for Google skills (1-hour expiry) | Integration test: expire token artificially, confirm refresh |
| M5.8.8 | `credentials_list` tool returns inventory without token material | Confirmed no token bytes in response |
| M5.8.9 | Disconnect removes credential row and confirms to user | SQLite row absent after disconnect command |
| M5.8.10 | Full multi-skill scenario: agent reads GitHub issues, creates calendar event | End-to-end demo with real APIs |

### Infrastructure & Deployment (M5.9–M5.12)

| Milestone | Description | Done When |
|---|---|---|
| M5.9 | `skill-web-search` container builds and responds to MCP tool calls | Search result returned via MCP |
| M5.10 | `docker compose up` starts all services, Caddy proxies correctly | Full stack running on local Docker |
| M5.11 | Multi-stage Dockerfile builds produce <30MB image | `docker images` confirms size |
| M5.12 | Deployed to Hetzner VPS, systemd service starts on boot | `systemctl status edgeclaw` shows active |
| M5.13 | Telegram message triggers agent turn end-to-end | Real Telegram message gets a real reply |

### TUI — Setup Wizard (M5.14)

_Detail: [TUI Spec](EDGECLAW_TUI_SPEC.md) Part 1_

| Milestone | Description | Done When |
|---|---|---|
| M5.14.1 | `clap` entry point: `setup`, `manage`, `--help` all parse correctly | `cargo test` for CLI parsing passes |
| M5.14.2 | Stage 1: SSH connection prompt, live connectivity verify, Docker check | Manual run with real VPS succeeds |
| M5.14.3 | Stage 2: Agent name and Telegram bot token collected and verified | Bot token verified via Telegram API |
| M5.14.4 | Stage 3: Model selection renders table, API key verified with 1-token call | Live verify against Anthropic API passes |
| M5.14.5 | Stage 4: Multi-select skill picker, OAuth credential collection per skill | All three OAuth flows collect correctly |
| M5.14.6 | Pre-deployment summary renders all collected config | Visual review confirms all fields present |
| M5.14.7 | Deployment SSHs to VPS, rsyncs env files, runs `docker compose up -d` | Full end-to-end deploy from `edgeclaw setup` |
| M5.14.8 | `edgeclaw.toml` written correctly, no secrets in file | Config file audit: no secret values present |

### TUI — Management Dashboard (M5.15)

_Detail: [TUI Spec](EDGECLAW_TUI_SPEC.md) Part 2_

| Milestone | Description | Done When |
|---|---|---|
| M5.15.1 | `ratatui` management TUI launches with Status screen live data | Status screen renders with real `/admin/status` data |
| M5.15.2 | Skills screen shows installed skills with OAuth status | skill-gmail and skill-github shown correctly |
| M5.15.3 | Logs screen streams live server logs with filter | Logs visible within 5s of a Telegram message |
| M5.15.4 | Settings screen edits and redeploys a changed field | Model change reflected in running container |
| M5.15.5 | Secrets screen rotates ANTHROPIC_API_KEY via SSH env update | Only target secret updated, container restarted |
| M5.15.6 | `TOKEN_MASTER_KEY` rotation warning shown before rotating | Warning text visible when row selected |
