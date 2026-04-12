# EdgeClaw — Roadmap

> Focused roadmap for the next iterations of EdgeClaw.
>
> _Last updated: 2026-04-12. Previous iteration: WebSocket sessions and
> non-blocking agent turns (PR #22)._

---

## Current State

EdgeClaw is a self-hosted personal AI agent runtime on tokio + axum + sqlx +
Docker Compose. The core loop works: users send messages via WebSocket or HTTP,
the agent runs a tool-use loop against the Anthropic API, and skills execute via
MCP over HTTP. WebSocket sessions support interactive tool approval flows, and
agent turns run in non-blocking background tasks.

**What's implemented:**

- Agent tool-use loop with inline execution and permission policies (`agent-core`)
- Built-in tools: `bash`, `file_read`, `file_write`, `file_edit`, `glob`, `grep`
- MCP skill registry with namespaced tool routing
- WebSocket sessions with human-in-the-loop tool approval
- Non-blocking `run_agent_turn` via `tokio::spawn`
- SQLite persistence (messages, skills, credentials, scheduled tasks, prefs)
- Credential encryption (AES-256-GCM + HKDF)
- Docker Compose deployment with Caddy TLS termination
- Skills: web-search, http-fetch, gmail, github, google-calendar

---

## Phase 1 — CLI and TUI

With WebSocket support in place, we can provide a terminal-based chat interface
that connects to the running server.

### 1.1 CLI Chat Client

A `chat` subcommand on `edgeclaw-cli` that opens a WebSocket connection to the
server and provides an interactive REPL-style chat flow, built with `ratatui`.

- Connect to `ws://<host>/ws` with user ID handshake
- Inline terminal output — conversation stays in scrollback history
- Render agent responses as they arrive
- Handle tool approval prompts inline (y/n confirmation in terminal)
- Support `Ctrl+C` graceful disconnect

### 1.2 Setup Wizard (from TUI spec)

The `edgeclaw setup` wizard using `inquire` for first-run configuration:

- Stage 1: VPS connection (SSH host, user, key)
- Stage 2: Agent identity (name, Telegram bot token)
- Stage 3: LLM model selection and API key verification
- Stage 4: Skill selection and OAuth credential collection
- Pre-deployment summary and SSH-based deploy

### 1.3 Management Dashboard

The `edgeclaw manage` persistent `ratatui` dashboard:

- Status screen with live data from `/admin/status`
- Skills screen with OAuth status
- Logs screen with live streaming and filters
- Settings and secrets management

**Specs:** [TUI Spec](EDGECLAW_TUI_SPEC.md)

---

## Phase 2 — Agent Soul (SOUL)

Customize the agent's name and personality. The soul is a persisted identity
that shapes how the agent communicates.

### 2.1 Bones — Behavioral Archetypes

Predefined archetype configurations that provide system prompt fragments:

- Archetypes: Assistant, Engineer, Researcher, Operator, Mentor
- Tone: Neutral, Friendly, Direct, Formal
- Verbosity: Terse, Balanced, Thorough
- Decision style: Cautious, Balanced, Autonomous

### 2.2 Soul — Persisted Identity

User-customizable identity stored in the database:

- Name (e.g. "Atlas", "Kai")
- Personality description (free-text)
- Selected archetype and trait overrides
- Injected into system prompt on every turn

### 2.3 Soul Management

- `SOUL.md` file format for human-readable editing
- REST API: `POST /soul`, `PATCH /soul`, `GET /soul`
- LLM-assisted generation: `POST /soul/generate`
- CLI command: `edgeclaw soul` to view/edit at any time

---

## Phase 3 — Sub-Agents and Orchestration

Allow an agent to spawn child agents for complex, multi-step tasks.

### 3.1 Sub-Agent Spawning

A `spawn_agent` built-in tool that creates a new `Agent` instance:

- Fresh message history (task as first user message)
- Filtered tool set from parent
- Own iteration budget and cancellation token
- Shared read access to `SkillRegistry`

**Agent type presets:**

| Type | Tools | Purpose |
|------|-------|---------|
| `explorer` | `file_read`, `glob`, `grep`, `bash` (read-only) | Research, code exploration |
| `worker` | All built-in + MCP skills | Execute changes |
| `planner` | None (text-only) | Plan and decompose tasks |

### 3.2 Execution Model

- **Sync** (default): Parent blocks until child completes. Result returned as
  tool result.
- **Async**: Child runs in background `tokio::spawn`. Parent uses `send_message`
  to communicate, receives completion notification.

### 3.3 Agent Registry

Track running agents for `send_message` and `stop_agent` tools. Persist agent
instance state in SQLite for crash recovery.

### 3.4 Swarm Coordination

Coordinator agent spawns multiple workers and orchestrates collaboration:

- Coordinator has `spawn_agent`, `send_message`, `stop_agent`
- Workers cannot spawn further agents (prevents recursion)
- Optional shared scratchpad directory for passing artifacts

---

## Deferred

The following features are parked for future consideration. Their specs are
archived in `docs/archive/`.

- **Embeddings and RAG** — Semantic search over memory facts and documents,
  automatic context injection, background consolidation
- **SurrealDB migration** — Replace SQLite + sqlx with SurrealDB embedded mode
  for native graph queries and vector search
- **Temporal knowledge graph** — Versioned entities with confidence decay, drift
  detection, and self-healing (depends on SurrealDB)
- **Auto-compaction and streaming** — Context window management via
  summarization, SSE streaming from agent to client
