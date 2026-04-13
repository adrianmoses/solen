# EdgeClaw — Roadmap

> Focused roadmap for the next iterations of EdgeClaw.
>
> _Last updated: 2026-04-14. Previous iteration: CLI crate with `serve`
> and `chat` commands._

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
- CLI crate (`edgeclaw-cli`) with `serve` and `chat` commands

---

## Phase 1 — CLI

The CLI provides three top-level verbs that cover the full local workflow:
starting the server, chatting with the agent, and managing configuration.
All persistent state lives in `~/.config/edgeclaw/config.toml`.

### 1.1 Server Management (`edgeclaw serve`) ✓

Start, stop, and monitor the WebSocket server locally or as a background daemon:

- ✓ `edgeclaw serve` starts the server in foreground (default `127.0.0.1:7100`)
- ✓ `--host` and `--port` flags override defaults
- `--daemonize` forks into background; `--pid-file` for process management
- Lifecycle subcommands: `serve status`, `serve stop`, `serve restart`
- Optional TLS termination via `--tls-cert` / `--tls-key`

### 1.2 Chat Client (`edgeclaw chat`)

REPL-like chat session with spawn-or-attach behavior — zero-config entry point:

- ✓ Attempts WebSocket connect to `--connect` (default `ws://127.0.0.1:7100/ws`)
- ✓ If no server is running, spawns one in-process and tears it down on exit
- ✓ Inline terminal UI with crossterm raw mode (no alternate screen)
- ✓ Tool approval prompts (`y`/`n`) rendered inline
- ✓ Colored output: `agent>`, `error>`, `[tool]` prefixes
- ✓ Ctrl-C / Ctrl-D graceful disconnect
- Pipe mode (`--no-tui`) for scripting: `echo "prompt" | edgeclaw chat --no-tui`
- Named sessions via `--session-id`
- Agent personality selection via `--agent <name>`
- Multiline input via Ctrl-J
- `[a]` approve-all-in-session option

### 1.3 Configuration (`edgeclaw config`)

Read, write, and interactively manage agent configuration:

- First-run wizard: guided setup for model, personality, and connectors
- `config show` / `config edit` for viewing and direct TOML editing
- `config set model` — provider, model ID, API key, temperature
- `config set personality` — named personalities with system prompts
- `config set approval` — tool approval mode (always-ask, auto-approve, allowlist)
- `config set tools` — enable/disable individual tools
- `config connector add|list|remove|test` — Telegram, Discord, Slack connectors

**Specs:** [CLI Spec](edgeclaw-cli-spec.md)

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
