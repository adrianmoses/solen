# EdgeClaw

Stateful, edge-native AI agent runtime on Cloudflare Workers + Durable Objects. Each user gets a dedicated agent instance with its own SQLite database, running a ReAct loop against the Anthropic API.

## Architecture

```
HTTP / WebSocket ──> Dispatcher Worker ──> AgentDO (Rust WASM)
                                               │
                         ┌─────────┬───────────┴──────────┐
                         ▼         ▼                      ▼
                   MemorySkill  WebSearch             HttpFetch
                   (skill-memory) (skill-web-search) (skill-http-fetch)
```

**Workspace crates:**
- `crates/agent-core` — Pure Rust: ReAct agent loop, LLM client, domain types, `ToolExecutor` trait. Zero workers-rs dependency.
- `crates/mcp-client` — JSON-RPC 2.0 MCP client for connecting to skill servers.
- `crates/skill-registry` — Namespaced tool routing (`skill:tool`), implements `ToolExecutor`.
- `crates/edgeclaw-worker` — workers-rs glue: Durable Object with SQLite, dispatcher, WebSocket, skill management, human-in-the-loop.

**Skill workers** (independent deployments in `skills/`):
- `skills/skill-memory` — Key-value memory store with tags (Durable Object + SQLite)
- `skills/skill-web-search` — Web search via Brave Search API
- `skills/skill-http-fetch` — URL fetcher with HTML stripping

## Prerequisites

- Rust (stable) with `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- Node.js 20+
- [wrangler](https://developers.cloudflare.com/workers/wrangler/) (`npm install`)

## Local Development

### 1. Set up your API key

```bash
cp .dev.vars.example .dev.vars
# Edit .dev.vars and add your Anthropic API key
```

### 2. Start the local dev server

```bash
npx wrangler dev
```

This starts the main worker at `http://localhost:8787`.

### 3. Test with curl

```bash
# Send a message
curl -X POST http://localhost:8787/message \
  -H "Content-Type: application/json" \
  -H "X-User-Id: test-user" \
  -d '{"message": "Hello, what can you do?"}'

# View conversation history
curl http://localhost:8787/history -H "X-User-Id: test-user"

# List registered skills (empty initially)
curl http://localhost:8787/skills -H "X-User-Id: test-user"
```

### 4. Register a skill (requires skill worker running)

To test with skills, you need to deploy or run a skill worker first:

```bash
# In a separate terminal, run a skill worker locally
cd skills/skill-memory
cp ../../.dev.vars.example .dev.vars  # if needed
npx wrangler dev --port 8788

# Back in the main terminal, register the skill
curl -X POST http://localhost:8787/skills/add \
  -H "Content-Type: application/json" \
  -H "X-User-Id: test-user" \
  -d '{"name": "memory", "url": "http://localhost:8788"}'

# Register a skill that requires auth
curl -X POST http://localhost:8787/skills/add \
  -H "Content-Type: application/json" \
  -H "X-User-Id: test-user" \
  -d '{"name": "authed", "url": "https://skill.example.com", "auth_header_name": "x-api-key", "auth_header_value": "sk-secret"}'

# Now messages can trigger memory tools
curl -X POST http://localhost:8787/message \
  -H "Content-Type: application/json" \
  -H "X-User-Id: test-user" \
  -d '{"message": "Remember that my favorite color is blue"}'
```

### 5. Human-in-the-loop approvals

Destructive tool calls (names containing "delete", "remove", "send", "drop") require approval:

```bash
# Check pending approvals
curl http://localhost:8787/approvals -H "X-User-Id: test-user"

# Approve a pending tool call
curl -X POST http://localhost:8787/approve \
  -H "Content-Type: application/json" \
  -H "X-User-Id: test-user" \
  -d '{"id": 1, "approve": true}'

# Deny a pending tool call
curl -X POST http://localhost:8787/approve \
  -H "Content-Type: application/json" \
  -H "X-User-Id: test-user" \
  -d '{"id": 1, "approve": false}'
```

### WebSocket

```bash
# Connect via WebSocket (using websocat or similar)
websocat ws://localhost:8787/?user_id=test-user

# Send messages as JSON
{"message": "Hello"}

# Approve/deny via WebSocket
{"type": "approve", "id": 1}
{"type": "deny", "id": 1}
```

## Testing

```bash
# Rust unit tests (17 tests across agent-core, mcp-client, skill-registry)
cargo test --workspace

# Integration tests (requires worker build)
cargo install worker-build
worker-build --release crates/edgeclaw-worker
npm install
npm test
```

## Secrets & Environment Variables

### Required secrets

Set via `wrangler secret put <NAME>` for production, or in `.dev.vars` for local development.

| Secret | Worker | Description |
|--------|--------|-------------|
| `ANTHROPIC_API_KEY` | `edgeclaw` (main) | Anthropic API key for LLM calls |
| `SKILL_ENCRYPTION_KEY` | `edgeclaw` (main) | AES-256-GCM key for encrypting per-skill auth credentials stored in Durable Object SQLite. Use a random 32+ character string (e.g. `openssl rand -base64 32`). If unset, skill auth values are stored in plaintext. |
| `BRAVE_SEARCH_API_KEY` | `skill-web-search` | Brave Search API key |

### Optional environment variables

Set in `wrangler.toml` `[vars]` or `.dev.vars`.

| Variable | Default | Description |
|----------|---------|-------------|
| `CLAUDE_MODEL` | `claude-sonnet-4-20250514` | Anthropic model ID |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` | Anthropic API base URL |

## Deployment

### Main worker

```bash
npx wrangler deploy
npx wrangler secret put ANTHROPIC_API_KEY
npx wrangler secret put SKILL_ENCRYPTION_KEY
```

### Skill workers

Each skill is deployed independently from its directory:

```bash
# Memory skill
cd skills/skill-memory && npx wrangler deploy

# Web search skill (requires Brave Search API key)
cd skills/skill-web-search && npx wrangler deploy
npx wrangler secret put BRAVE_SEARCH_API_KEY

# HTTP fetch skill
cd skills/skill-http-fetch && npx wrangler deploy
```

After deploying skills, register them with the agent via `POST /skills/add` with the deployed Worker URL.

## API Reference

All endpoints require user identity via `X-User-Id` header or `?user_id=` query param.

### POST /message
Send a message to the agent. Triggers the ReAct loop with tool execution.

```json
// Request
{ "message": "Hello, what can you do?" }

// Response (no tools)
{ "answer": "I can help you with...", "pending_tool_calls": [] }

// Response (awaiting approval for destructive tool)
{ "status": "awaiting_approval", "answer": null, "pending_approvals": [...] }
```

### GET /history
Retrieve conversation history (last 50 messages).

### POST /skills/add
Register an MCP skill server. Connects, initializes, and discovers available tools.

```json
// Request (no auth)
{ "name": "memory", "url": "https://skill-memory.your-account.workers.dev" }

// Request (with per-skill auth)
{
  "name": "authed-skill",
  "url": "https://skill.example.com",
  "auth_header_name": "authorization",
  "auth_header_value": "Bearer sk-secret-token"
}

// Response
{ "skill": "memory", "tools": ["memory:memory_store", "memory:memory_retrieve", ...] }
```

`auth_header_name` defaults to `"authorization"` if only `auth_header_value` is provided. Auth credentials are encrypted at rest with AES-256-GCM using the `SKILL_ENCRYPTION_KEY` secret.

### GET /skills
List all registered skills with their cached tool definitions. Auth header values are masked in the response.

### POST /approve
Approve or deny a pending destructive tool call.

```json
{ "id": 1, "approve": true }
```

### GET /approvals
List all pending approval requests.

### GET / (WebSocket Upgrade)
Connect via WebSocket for real-time interaction. Send `Upgrade: websocket` header.

### POST /orchestrate
Fan out a task to multiple named agents.

```json
// Request
{ "task": "Research Rust WASM", "agents": ["researcher", "writer"] }

// Response
{ "researcher": { "answer": "..." }, "writer": { "answer": "..." } }
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
│   └── edgeclaw-worker/src/   # Cloudflare Worker
│       └── lib.rs             # Dispatcher, AgentDO, WebSocket, skills, approvals
├── skills/
│   ├── mcp-server-util/       # Shared JSON-RPC server helpers
│   ├── skill-memory/          # Memory skill (DO + SQLite)
│   ├── skill-web-search/      # Brave Search skill
│   └── skill-http-fetch/      # URL fetch skill
├── tests/
│   ├── fixtures/              # JSON fixtures for unit tests
│   └── integration/           # Miniflare integration tests
├── wrangler.toml              # Main worker config
├── .dev.vars.example          # Environment variables template
└── CLAUDE.md                  # Development conventions
```
