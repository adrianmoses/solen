# EdgeClaw

Personal AI agent runtime, self-hosted on a VPS via tokio + axum + sqlx + Docker Compose. Each agent runs a ReAct loop against the Anthropic API with extensible skills via MCP.

## Architecture

```
WebSocket/HTTP ──> axum server ──> Agent (ReAct loop)
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
- `crates/edgeclaw-server` — axum + sqlx host with WebSocket sessions, scheduler, and human-in-the-loop tool approvals.
- `crates/edgeclaw-cli` — CLI binary (`edgeclaw`) with `serve`, `chat`, `config`, and `soul` commands.

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
# Via the CLI (default: 127.0.0.1:7100)
cargo run -p edgeclaw-cli -- serve

# Or directly via the server crate (default: 0.0.0.0:8080)
cargo run -p edgeclaw-server
```

### 3. Chat with the agent

The CLI provides a terminal chat UI that auto-starts the server if needed:

```bash
cargo run -p edgeclaw-cli -- chat
```

This opens an inline REPL with colored output, tool approval prompts, and graceful disconnect via Ctrl-C/Ctrl-D.

### 4. Connect via WebSocket

The primary interface is a WebSocket connection at `ws://localhost:8080/ws`. This enables bidirectional communication with the agent, including interactive tool approval prompts.

Using [websocat](https://github.com/vi/websocat):

```bash
websocat ws://localhost:8080/ws
```

Send a handshake with your user ID, then start chatting:

```json
{"user_id": "default"}
{"type": "user_message", "message": "Hello, what can you do?"}
```

The server responds with typed messages:

```json
{"type": "session_started", "session_id": "..."}
{"type": "agent_response", "answer": "I can help you with..."}
```

#### Tool approval flow

When the agent calls a tool that requires permission (e.g., `file_write`, `bash`), the server sends a confirmation prompt instead of executing it immediately:

```json
{"type": "confirmation_prompt", "request_id": "abc-123", "tool_calls": [...], "reasons": ["..."]}
```

Respond with:

```json
{"type": "approval_response", "request_id": "abc-123", "approved": true}
```

The agent loop stays alive throughout — it blocks on your response and continues once you approve or deny. Denied tools return an error to the LLM so it can adjust. If the WebSocket disconnects or you don't respond within 5 minutes, tools are auto-denied.

#### HTTP fallback

`POST /message` still works for simple use cases. It auto-approves all tools (no approval prompts):

```bash
curl -X POST http://localhost:8080/message \
  -H "Content-Type: application/json" \
  -d '{"user_id": "default", "message": "Hello, what can you do?"}'
```

Other useful HTTP endpoints:

```bash
# Health check
curl http://localhost:8080/health

# View conversation history
curl http://localhost:8080/history?user_id=default

# Clear conversation history
curl -X DELETE http://localhost:8080/history?user_id=default

# List registered skills
curl http://localhost:8080/skills?user_id=default
```

### 5. Configure the agent

```bash
# First-run wizard
cargo run -p edgeclaw-cli -- config

# Set model and API key
cargo run -p edgeclaw-cli -- config set model --model claude-sonnet-4-20250514 --api-key sk-ant-...

# Set approval mode
cargo run -p edgeclaw-cli -- config set approval --mode auto-approve

# Show current config
cargo run -p edgeclaw-cli -- config show
```

### 6. Manage the agent's soul

The soul defines the agent's identity and personality. It is injected into the system prompt on every turn.

```bash
# View current soul
cargo run -p edgeclaw-cli -- soul

# Set individual fields
cargo run -p edgeclaw-cli -- soul set --name Atlas --archetype engineer --tone direct

# Open soul as SOUL.md in your editor
cargo run -p edgeclaw-cli -- soul edit

# Generate a soul from a description (uses the LLM)
cargo run -p edgeclaw-cli -- soul generate "a sarcastic DevOps expert who loves automation"

# Import from a SOUL.md file
cargo run -p edgeclaw-cli -- soul import my-agent.soul.md

# Export current soul to stdout
cargo run -p edgeclaw-cli -- soul export
```

**SOUL.md format:**

```markdown
---
name: Atlas
archetype: engineer
tone: direct
verbosity: balanced
decision_style: autonomous
---

A sharp, systems-minded engineer who thinks in first principles...
```

Available archetypes: `assistant`, `engineer`, `researcher`, `operator`, `mentor`
Tone: `neutral`, `friendly`, `direct`, `formal`
Verbosity: `terse`, `balanced`, `thorough`
Decision style: `cautious`, `balanced`, `autonomous`

**REST API:**

```bash
# Get soul
curl http://localhost:8080/soul?user_id=default

# Create/replace soul
curl -X POST http://localhost:8080/soul \
  -H "Content-Type: application/json" \
  -d '{"user_id": "default", "name": "Atlas", "archetype": "engineer", "tone": "direct"}'

# Partial update
curl -X PATCH http://localhost:8080/soul \
  -H "Content-Type: application/json" \
  -d '{"user_id": "default", "tone": "friendly"}'

# LLM-assisted generation
curl -X POST http://localhost:8080/soul/generate \
  -H "Content-Type: application/json" \
  -d '{"user_id": "default", "description": "a cheerful research assistant"}'
```

### 7. Register a skill

```bash
# Start skills via Docker Compose
docker compose up -d

# Register a skill
curl -X POST http://localhost:8080/skills/add \
  -H "Content-Type: application/json" \
  -d '{"user_id": "default", "name": "memory", "url": "http://localhost:8788"}'
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
│   │   ├── soul.rs            # Soul types, prompt composer, SOUL.md parsing
│   │   ├── types.rs           # Domain types + ToolExecutor trait
│   │   └── error.rs           # AgentError enum
│   ├── mcp-client/src/        # MCP protocol client
│   │   ├── protocol.rs        # JSON-RPC 2.0 types
│   │   └── client.rs          # McpClient (initialize, list_tools, call_tool)
│   ├── skill-registry/src/    # Skill routing layer
│   │   └── lib.rs             # SkillRegistry, SkillRow, namespaced dispatch
│   ├── edgeclaw-server/src/   # axum + sqlx server
│   │   ├── main.rs            # Entry point
│   │   ├── server.rs          # Router, AppState, ServerConfig
│   │   ├── agent.rs           # Agent turn harness, soul loading
│   │   └── session.rs         # WebSocket sessions, approval channels
│   └── edgeclaw-cli/src/      # CLI binary
│       ├── main.rs            # Command dispatch (serve, chat, config, soul)
│       ├── chat/              # Terminal chat UI
│       ├── config/            # Config management (model, personality, tools)
│       └── soul.rs            # Soul subcommands (show, set, edit, generate, import, export)
├── skills/
│   ├── mcp-server-util/       # Shared JSON-RPC server helpers
│   ├── skill-memory/          # Memory skill
│   ├── skill-web-search/      # Brave Search skill
│   └── skill-http-fetch/      # URL fetch skill
├── .env.example               # Environment variables template
├── docker-compose.yml         # Production deployment
└── CLAUDE.md                  # Development conventions
```
