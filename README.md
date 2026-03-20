# EdgeClaw

Personal AI agent runtime, self-hosted on a VPS via tokio + axum + sqlx + Docker Compose. Each agent runs a ReAct loop against the Anthropic API with extensible skills via MCP.

## Architecture

```
HTTP ──> axum server ──> Agent (ReAct loop)
              │                │
              ▼          ┌─────┴──────────┬──────────┐
           SQLite        ▼                ▼          ▼
                    MemorySkill     WebSearch    HttpFetch
                   (Docker svc)   (Docker svc) (Docker svc)
```

**Workspace crates:**
- `crates/agent-core` — Pure Rust: ReAct agent loop, LLM client, domain types, `ToolExecutor` trait. Zero framework dependency.
- `crates/mcp-client` — JSON-RPC 2.0 MCP client for connecting to skill servers.
- `crates/skill-registry` — Namespaced tool routing (`skill:tool`), implements `ToolExecutor`.
- `crates/edgeclaw-server` — axum + sqlx host with scheduler, human-in-the-loop approvals.

**Skills** (independent Docker services in `skills/`):
- `skills/skill-memory` — Key-value memory store with tags
- `skills/skill-web-search` — Web search via Brave Search API
- `skills/skill-http-fetch` — URL fetcher with HTML stripping

## Prerequisites

- Rust (stable)
- Docker & Docker Compose (for skills and production deployment)

## Local Development

### 1. Set up environment

```bash
cp .env.example .env
# Edit .env and add your Anthropic API key
```

### 2. Start the server

```bash
cargo run -p edgeclaw-server
```

The server starts at `http://localhost:8080`.

### 3. Test with curl

```bash
# Health check
curl http://localhost:8080/health

# Send a message
curl -X POST http://localhost:8080/message \
  -H "Content-Type: application/json" \
  -d '{"message": "Hello, what can you do?"}'

# View conversation history
curl http://localhost:8080/history

# List registered skills
curl http://localhost:8080/skills
```

### 4. Register a skill

```bash
# Start skills via Docker Compose
docker compose up -d

# Register a skill
curl -X POST http://localhost:8080/skills/add \
  -H "Content-Type: application/json" \
  -d '{"name": "memory", "url": "http://localhost:8788"}'
```

### 5. Human-in-the-loop approvals

Destructive tool calls (names containing "delete", "remove", "send", "drop") require approval:

```bash
# Check pending approvals
curl http://localhost:8080/approvals

# Approve a pending tool call
curl -X POST http://localhost:8080/approve \
  -H "Content-Type: application/json" \
  -d '{"id": 1, "approve": true}'
```

## Testing

```bash
# Unit tests
cargo test --workspace

# Clippy
cargo clippy -p agent-core -- -D warnings
cargo clippy -p edgeclaw-server -- -D warnings

# Format check
cargo fmt --all -- --check
```

## Environment Variables

Configure via `.env` file (loaded automatically via dotenvy) or system environment variables.

### Required

| Variable | Description |
|----------|-------------|
| `ANTHROPIC_API_KEY` | Anthropic API key for LLM calls |

### Optional

| Variable | Default | Description |
|----------|---------|-------------|
| `CLAUDE_MODEL` | `claude-sonnet-4-20250514` | Anthropic model ID |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` | Anthropic API base URL |
| `DATABASE_URL` | `sqlite://edgeclaw.db?mode=rwc` | SQLite database URL |
| `HOST` | `0.0.0.0` | Server bind host |
| `PORT` | `8080` | Server bind port |
| `MAX_TASKS_PER_USER` | `20` | Max scheduled tasks per user |
| `SKILL_ENCRYPTION_KEY` | — | AES-256-GCM key for encrypting per-skill auth credentials |

Skills use a separate `.env.skills` file for their environment (e.g., `BRAVE_SEARCH_API_KEY`).

## Deployment

```bash
# Production (Docker Compose on VPS)
cp .env.example .env  # fill in secrets
docker compose up -d
```

## Project Structure

```
edgeclaw/
├── crates/
│   ├── agent-core/src/        # Pure Rust agent library
│   │   ├── agent.rs           # ReAct loop (run + resume)
│   │   ├── llm.rs             # Anthropic API client + HttpBackend trait
│   │   ├── types.rs           # Domain types + ToolExecutor trait
│   │   └── error.rs           # AgentError enum
│   ├── mcp-client/src/        # MCP protocol client
│   │   ├── protocol.rs        # JSON-RPC 2.0 types
│   │   └── client.rs          # McpClient (initialize, list_tools, call_tool)
│   ├── skill-registry/src/    # Skill routing layer
│   │   └── lib.rs             # SkillRegistry, SkillRow, namespaced dispatch
│   └── edgeclaw-server/src/   # axum + sqlx server
│       ├── main.rs            # Entry point
│       └── server.rs          # Router, AppState, ServerConfig
├── skills/
│   ├── mcp-server-util/       # Shared JSON-RPC server helpers
│   ├── skill-memory/          # Memory skill
│   ├── skill-web-search/      # Brave Search skill
│   └── skill-http-fetch/      # URL fetch skill
├── .env.example               # Environment variables template
├── docker-compose.yml         # Production deployment
└── CLAUDE.md                  # Development conventions
```
