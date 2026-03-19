# EdgeClaw — Prototype Specification

> A stateful, edge-native, WASM-isolated personal AI agent with MCP-native skill routing.

---

## Overview

EdgeClaw is a personal AI agent runtime built on three core principles derived from the lessons of OpenClaw and NanoClaw:

1. **Isolation by architecture, not policy** — WASM sandboxing is stronger than container-level security because there is no syscall surface and no kernel exposure. A compromised skill cannot touch the host or any other skill.
2. **Identity-first statefulness** — each agent instance is a Durable Object with a globally-unique identity, its own embedded SQLite database, and strongly consistent storage colocated with compute. The agent *is* the state — there is no "load from external store on every request" round-trip.
3. **Skills as first-class citizens** — tools are not hardcoded features. They are MCP-compatible modules discovered and invoked at runtime, each isolated from the others and from the agent core.

The entire stack is written in Rust. The dispatcher Worker and `AgentDO` Durable Object are implemented using `workers-rs`, which compiles to WASM and provides idiomatic Rust bindings to all Cloudflare runtime APIs including Durable Objects, SQLite storage, `Fetch`, service bindings, and WebSockets — no JavaScript required.

---

## Architecture Overview

```
                        ┌─────────────────────────────────────────┐
  Telegram / HTTP  ───▶ │  Dispatcher Worker  (Rust / workers-rs) │
                        │  stateless — routes by user ID          │
                        └─────────────────┬───────────────────────┘
                                          │  DO stub call
                                          ▼
                        ┌─────────────────────────────────────────┐
                        │  AgentDO  (Rust Durable Object)         │
                        │  identity: "agent:{user_id}"            │
                        │                                         │
                        │  SQLite tables:                         │
                        │    messages, skills, prefs,             │
                        │    pending_approvals                     │
                        │                                         │
                        │  Runs: ReAct loop (agent-core crate)    │
                        │  Holds: WebSocket connections           │
                        └──────────┬──────────────────────────────┘
                                   │  MCP over HTTP / SSE
                     ┌─────────────┼──────────────┐
                     ▼             ▼              ▼
              ┌────────────┐ ┌──────────┐ ┌────────────┐
              │MemorySkill │ │WebSearch │ │ HttpFetch  │  ... user-added
              │(Rust DO)   │ │(Rust     │ │(Rust       │      MCP servers
              │            │ │ Worker)  │ │ Worker)    │
              └────────────┘ └──────────┘ └────────────┘
```

The central insight: **the Durable Object is the agent**. It is not a thin wrapper that loads and saves state around a stateless function — it is a persistent actor with its own SQLite database, identity, and lifecycle. State is never lost and never needs to be round-tripped through an external store on the hot path.

---

## Repository Structure

```
edgeclaw/
├── crates/
│   ├── agent-core/          # Pure Rust ReAct loop and LLM client
│   ├── mcp-client/          # Phase 2 — MCP protocol client
│   ├── skill-registry/      # Phase 2 — skill discovery and dispatch
│   └── edgeclaw-worker/     # workers-rs entrypoint: AgentDO + Dispatcher
├── skills/
│   ├── skill-memory/        # Phase 2 — DO-backed memory MCP server (Rust)
│   ├── skill-web-search/    # Phase 2 — stateless MCP Worker (Rust)
│   └── skill-http-fetch/    # Phase 2 — stateless MCP Worker (Rust)
├── tests/
│   ├── integration/         # Miniflare-based end-to-end tests
│   └── fixtures/            # Recorded LLM responses for deterministic tests
└── docs/
    └── architecture.md
```

All crates target `wasm32-unknown-unknown`. `workers-rs` (`worker` crate) provides the Cloudflare runtime bindings. `worker-build` handles the WASM compilation and shim generation required by `wrangler`.

---

## workers-rs — Key Capabilities

`workers-rs` provides idiomatic Rust access to the full Cloudflare Workers platform:

- **`#[event(fetch)]`** — the dispatcher Worker entry point
- **`#[durable_object]`** macro — marks a Rust struct as a Durable Object class; `worker-build` generates the required JS glue automatically
- **`DurableObject` trait** — `fetch()` and alarm handler that the runtime calls into
- **`State::storage()`** — access to the DO's Storage API, including `sql()` for the SQLite backend
- **`SqlStorage::exec()`** — executes SQL against the DO's embedded SQLite database
- **`ObjectNamespace::id_from_name()`** — derives a deterministic DO identity from a string; critical for routing the same user to the same DO instance every time
- **`Stub::fetch()`** — sends an HTTP request to a remote DO instance
- **`Env::durable_object()`**, **`Env::secret()`**, **`Env::service()`** — access to bindings declared in `wrangler.toml`
- **`WebSocket` / `WebSocketPair`** — WebSocket hibernation support

> **Known issue:** As of April 2025 there is an open memory leak bug in `workers-rs` (issue #722) affecting Durable Object eviction — memory allocated for a DO is not freed on eviction in the Rust/WASM path, whereas the JS path is unaffected. Track this issue before production use; it may be resolved by the time implementation begins.

---

## Phase 1 — Rust Agent Core with Durable Objects

### Goal

A minimal, auditable agent loop running inside a Cloudflare Durable Object implemented entirely in Rust via `workers-rs`. The `AgentDO` owns all conversation state in its SQLite database. The `agent-core` crate contains the pure Rust ReAct logic with no runtime dependencies — it receives a fully-assembled `AgentContext` from the DO, runs the LLM loop, and returns results for the DO to persist and act on.

### Deliverables

- `agent-core` crate compiles to `wasm32-unknown-unknown` with zero workers-rs dependency
- `AgentDO` Durable Object in Rust with SQLite schema and conversation persistence
- Dispatcher Worker in Rust routing by user ID via `id_from_name()`
- ReAct loop making real Anthropic API calls via `worker::Fetch` from inside the DO
- WebSocket support on `AgentDO` for streaming-ready connections
- Local development and testing via Miniflare (full DO + SQLite emulation)

---

### 1.1 — Crate Boundaries

The separation between `agent-core` and `edgeclaw-worker` is strict and intentional:

**`agent-core`** — has zero dependency on `workers-rs` or any Cloudflare runtime crate. It defines the domain types (`Message`, `ContentBlock`, `ToolCall`, `ToolResult`, `ToolDefinition`, `AgentContext`), the LLM client trait and Anthropic implementation, and the ReAct loop. Its only async runtime requirement is a `HttpBackend` trait that the caller provides. This makes it independently testable with `reqwest` on native, and usable inside the DO via `worker::Fetch` on WASM.

**`edgeclaw-worker`** — depends on both `agent-core` and `worker` (workers-rs). It implements `AgentDO`, the dispatcher `fetch` handler, the SQLite schema, and all platform glue. This is the only crate that touches Cloudflare APIs.

---

### 1.2 — Domain Types (`agent-core`)

The core types represent a conversation as an ordered sequence of messages, where each message carries one or more typed content blocks. This matches the Anthropic Messages API wire format directly, simplifying serialization.

- **`Message`** — a single turn with `role` (user or assistant), a list of `ContentBlock`s, and a `created_at` timestamp for SQLite ordering.
- **`ContentBlock`** — a tagged enum covering `Text`, `ToolUse` (LLM requesting a tool call), and `ToolResult` (the response from a tool execution).
- **`AgentContext`** — the snapshot the DO assembles from SQLite before each run: conversation history, system prompt, and the list of available tool definitions.
- **`ToolDefinition`** — name, description, and JSON Schema for the tool's input; passed verbatim to the LLM.
- **`ToolCall`** — a parsed tool invocation from the LLM: id, name, and JSON input.
- **`ToolResult`** — the result of executing a tool: the matching `tool_use_id`, a content string, and an `is_error` flag.
- **`AgentRunResult`** — what the ReAct loop returns to the DO: newly generated messages to persist, an optional final answer string, and any pending tool calls that need execution.

---

### 1.3 — LLM Client (`agent-core`)

The LLM client is built around a `HttpBackend` trait with a single async `post` method. This decouples the client from any specific HTTP implementation:

- **Native / test target** — `reqwest` backend, used for unit and integration tests
- **WASM / DO target** — `worker::Fetch` backend, provided by `edgeclaw-worker`

The client serialises the `AgentContext` into an Anthropic Messages API request (including the tool definitions array), POSTs it, and deserialises the response into an `LlmResponse` containing a `StopReason` and a list of `ContentBlock`s.

The `base_url` field on `LlmConfig` is overridable for testing and for future Workers AI fallback routing.

---

### 1.4 — ReAct Agent Loop (`agent-core`)

The loop is a pure function — it takes an `AgentContext` and a user message, runs until the LLM either produces a final answer or requests tool calls, and returns an `AgentRunResult`. It never touches storage.

The loop runs up to `max_iterations` times (default: 10, configurable). On each iteration:

1. Call the LLM with the current context.
2. If `stop_reason` is `end_turn` — extract the text answer, append the assistant message to the new-messages list, and return with `answer` set.
3. If `stop_reason` is `tool_use` — extract the tool calls, append the assistant message to the new-messages list, and **return immediately** with `pending_tool_calls` set. The DO takes over from here: it persists the messages, executes the tools, and calls `agent.resume()` with the results.
4. If `stop_reason` is `max_tokens` or `stop_sequence` — return an error.

Returning to the DO at the tool-call boundary (rather than looping inline) is the key design decision. It allows the DO to persist intermediate state before any tool executes — meaning a DO eviction mid-run loses at most the current tool's execution, not the entire turn. It also makes human-in-the-loop approval straightforward: the DO can inspect `pending_tool_calls`, decide some require approval, persist them to the `pending_approvals` table, and respond to the user before ever executing them.

---

### 1.5 — AgentDO (`edgeclaw-worker`)

`AgentDO` is a Rust struct annotated with `#[durable_object]`. It holds a reference to the `State` provided by the runtime (which gives access to SQLite storage) and an `Env` for secrets and bindings.

**SQLite schema** — initialised once on first access:

```sql
CREATE TABLE IF NOT EXISTS messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    role       TEXT    NOT NULL,
    content    TEXT    NOT NULL,  -- serde_json of Vec<ContentBlock>
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS skills (
    name       TEXT PRIMARY KEY,
    url        TEXT NOT NULL,
    tools      TEXT NOT NULL,     -- serde_json of Vec<ToolDefinition>
    added_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_approvals (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    tool_call  TEXT    NOT NULL,  -- serde_json of ToolCall
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS prefs (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL
);
```

**`fetch()` handler** — the DO's HTTP entry point, implementing the `DurableObject` trait. Routes on path:

- `POST /message` — runs an agent turn; returns the final answer
- `POST /skills/add` — registers a new MCP skill URL (Phase 2)
- `GET /skills` — lists registered skills (Phase 2)
- `GET /history` — returns recent conversation history
- `GET /` with `Upgrade: websocket` — upgrades to a hibernating WebSocket

**Agent turn execution:**

1. Query SQLite for recent messages (bounded window, e.g. last 50), registered tools, and system prompt from prefs.
2. Assemble `AgentContext` and call `agent.run(ctx, user_message)`.
3. Persist `new_messages` to SQLite immediately.
4. If `pending_tool_calls` is non-empty:
   - Check for destructive calls. If any exist, persist them to `pending_approvals`, notify the user, and return — do not execute.
   - Otherwise execute all tool calls (Phase 1: no-op stubs; Phase 2: real MCP dispatch).
   - Persist tool result messages to SQLite.
   - Reassemble `AgentContext` from SQLite (now including results) and call `agent.resume()`.
   - Repeat until no pending tool calls remain.
5. Return the final answer.

**WebSocket hibernation** — `AgentDO` accepts WebSocket connections using the Workers hibernation API. The DO can sleep between messages without losing the connection, and `webSocketMessage` is called on wake. This is the foundation for streaming token delivery in Phase 3.

---

### 1.6 — Dispatcher Worker (`edgeclaw-worker`)

A `#[event(fetch)]` handler. Its sole responsibility is resolving a user identity from the incoming request and forwarding to the correct `AgentDO` via `id_from_name()`.

The user identity resolution strategy is pluggable and depends on the messaging frontend: for a Telegram webhook it is the Telegram user ID extracted from the JSON body; for a direct HTTP client it may come from an `Authorization` header. In Phase 1 this can be a simple header or query param for local testing.

Using `id_from_name()` rather than `new_unique_id()` is non-negotiable: `new_unique_id()` creates a new DO instance on every request, silently destroying all conversation history. `id_from_name()` produces a deterministic, globally consistent ID from a string.

---

### 1.7 — Multi-Agent Topology

DO-to-DO communication is native to the platform: any DO or Worker can obtain a `Stub` for any other DO via its binding and call `stub.fetch()`. This makes multi-agent composition a first-class primitive with no additional infrastructure.

```
┌──────────────────────────────────────┐
│  OrchestratorDO                      │
│  "orchestrator:task:{task_id}"       │
│                                      │
│  Decomposes task, fans out to        │
│  specialist DOs, synthesises results │
└───────┬─────────────────┬────────────┘
        │ stub.fetch()    │ stub.fetch()
        ▼                 ▼
┌──────────────┐   ┌──────────────┐
│ ResearchDO   │   │ WriterDO     │
│ "agent:      │   │ "agent:      │
│  researcher" │   │  writer"     │
│              │   │              │
│  web-search  │   │  http-fetch  │
│  skill       │   │  skill       │
└──────────────┘   └──────────────┘
```

Each sub-agent is an independent DO with its own SQLite, skill bindings, and conversation history. The platform guarantees single-threaded execution per DO, so there are no race conditions on state. Concurrent `stub.fetch()` calls from the orchestrator to different DOs execute in parallel on the platform side.

---

### 1.8 — wrangler.toml (Phase 1)

```toml
name = "edgeclaw"
main = "build/worker/shim.mjs"  # generated by worker-build
compatibility_date = "2026-01-01"

[build]
command = "cargo install -q worker-build && worker-build --release"

[[durable_objects.bindings]]
name = "AGENT_DO"
class_name = "AgentDo"  # matches the Rust struct name

[[migrations]]
tag = "v1"
new_sqlite_classes = ["AgentDo"]

# Set via: wrangler secret put ANTHROPIC_API_KEY
```

---

### 1.9 — Phase 1 Milestones

| Milestone | Description | Done When |
|---|---|---|
| M1.1 | `agent-core` compiles to `wasm32-unknown-unknown` with no workers-rs dependency | `cargo build --target wasm32-unknown-unknown` clean |
| M1.2 | SQLite schema initialises and messages round-trip correctly | Miniflare integration test persists and reloads messages |
| M1.3 | LLM client makes real Anthropic API calls via `worker::Fetch` | Single-turn smoke test passes against real API |
| M1.4 | ReAct loop handles multi-turn conversation including tool-call boundary | Multi-turn fixture test passes end-to-end |
| M1.5 | Dispatcher routes two different users to independent DO instances | Separate conversation histories confirmed |
| M1.6 | WebSocket connection survives DO sleep/wake cycle | Hibernation test with Miniflare passes |
| M1.7 | Orchestrator DO spawns two sub-agent DOs and collects results | Two-agent fan-out smoke test passes |
| M1.8 | `wrangler deploy` and real end-to-end HTTP message | Deployed worker returns a correct response |

---

### 1.10 — Phase 1 Dependencies

```toml
# crates/agent-core/Cargo.toml
[dependencies]
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
async-trait = "0.1"
thiserror   = "1"

# HTTP backend — feature-flagged
[features]
native = ["reqwest", "tokio"]

[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
reqwest = { version = "0.12", features = ["json"], optional = true }
tokio   = { version = "1", features = ["rt", "macros"], optional = true }
```

```toml
# crates/edgeclaw-worker/Cargo.toml
[dependencies]
agent-core  = { path = "../agent-core" }
worker      = { version = "0.5", features = ["http"] }
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"

[lib]
crate-type = ["cdylib"]
```

```toml
# dev tooling (package.json / npm)
# wrangler    ^3   — deploy and local dev
# miniflare   ^3   — full DO + SQLite emulation for integration tests
# vitest      ^2   — test runner (Miniflare tests are written in JS/TS
#                    per workers-rs documentation guidance)
```

> **Testing note:** `workers-rs` documentation explicitly states that Miniflare-based integration tests must be written in JavaScript or TypeScript, since Miniflare is a Node package. Unit tests for `agent-core` logic can and should be written in Rust using `cargo test` with the `native` feature. End-to-end DO tests use the Miniflare harness from a small JS test file.

---

## Phase 2 — Skills & MCP Support

### Goal

Replace the Phase 1 no-op tool executor with a live `SkillRegistry` backed by the `AgentDO`'s SQLite `skills` table. Each skill is a remote MCP server — either a bundled Cloudflare Worker (for reference skills) or a user-provided URL. The `mcp-client` crate handles MCP protocol communication over HTTP/SSE, compilable to WASM. Stateful skills like `skill-memory` are themselves Durable Objects in Rust, giving them the same identity and persistence guarantees as the agent.

### Deliverables

- `mcp-client` crate — MCP protocol client, WASM-compatible, no workers-rs dependency
- `skill-registry` crate — skill registration, discovery, and dispatch backed by DO SQLite
- Reference skills: `skill-memory` (Rust DO), `skill-web-search` (Rust Worker), `skill-http-fetch` (Rust Worker)
- Dynamic skill addition at runtime — user provides MCP URL in chat; tools appear on the next turn without redeployment
- Human-in-the-loop pause before destructive tool calls

---

### 2.1 — MCP Client (`mcp-client`)

The `mcp-client` crate implements the [Model Context Protocol](https://modelcontextprotocol.io) over HTTP and SSE. Like `agent-core`, it has no workers-rs dependency and uses a `HttpBackend` trait for transport, allowing it to be tested natively and run on WASM.

Responsibilities:

- **Connect** — POST to the MCP server's initialise endpoint, exchange capabilities
- **List tools** — GET the server's tool manifest; deserialise into `Vec<ToolDefinition>`
- **Call tool** — POST a tool invocation; deserialise the result into `ToolCallResult`
- **Reconnect** — detect dropped connections and re-initialise transparently

The `SkillRegistry` owns one `McpClient` per registered skill and is responsible for keeping connections alive for the duration of a DO invocation.

---

### 2.2 — Skill Registry (`skill-registry`)

The `SkillRegistry` is not a persistent struct — it is assembled from the `AgentDO`'s SQLite `skills` table at the start of each turn and discarded at the end. Each row in `skills` contains the skill name, URL, and a JSON-serialised `Vec<ToolDefinition>` cached from the last successful connection.

Responsibilities:

- **`from_db_rows()`** — reconnect to each registered skill's MCP URL and build the in-memory index
- **`register()`** — connect to a new URL, discover its tools, return a row for the caller to persist to SQLite
- **`all_tools()`** — return the union of all tool definitions across all skills, namespaced by skill name to avoid collisions
- **`dispatch()`** — route a `ToolCall` to the correct skill's `McpClient` and return a `ToolResult`

The registry implements the `ToolExecutor` trait defined in `agent-core`, so `AgentDO` can pass it directly to the agent loop without any coupling to MCP details.

**Dynamic registration flow:**

When the user says "add this MCP server", `AgentDO` calls `registry.register(name, url)`, receives the discovered tools, writes the row to SQLite immediately (strongly consistent — no propagation lag), and replies confirming the available tools. On the very next turn, `from_db_rows()` includes the new row and the LLM sees the new tools in its context.

---

### 2.3 — Skill Isolation Model

Each skill is a separate Worker or Durable Object. The only communication channel between a skill and `AgentDO` is HTTP — `AgentDO` calls the skill's MCP endpoint over the network. The Workers V8 isolate boundary enforces this: there is no shared memory, no shared file system, no shared process.

```
┌──────────────────────────────────────────────────────────┐
│  AgentDO  (Rust Durable Object)                          │
│  Single-threaded, strongly consistent                    │
│                                                          │
│  SkillRegistry                                           │
│       │                                                  │
│       │  MCP over HTTP / SSE — V8 isolate boundary      │
│       │  No shared memory possible                       │
│       │                                                  │
│  ┌────┴──────┐   ┌────────────┐   ┌──────────────────┐  │
│  │skill-mem  │   │skill-srch  │   │ skill-http-fetch │  │
│  │           │   │            │   │                  │  │
│  │ Rust DO   │   │ Rust Worker│   │ Rust Worker      │  │
│  │ SQLite    │   │ (stateless)│   │ (stateless)      │  │
│  │ per-user  │   │            │   │ + URL allowlist  │  │
│  └───────────┘   └────────────┘   └──────────────────┘  │
└──────────────────────────────────────────────────────────┘
```

Isolation guarantees:

- **No shared memory** — V8 isolate boundary between agent and all skills; enforced by the platform
- **No filesystem access** — Cloudflare Workers have no disk
- **Per-user skill state** — `skill-memory` is keyed `memory:{user_id}`, giving each user a fully isolated memory store
- **Blast radius contained** — a compromised or buggy skill can only affect its own MCP response, not the agent's SQLite or another skill's state
- **Single-threaded DO serialisation** — concurrent requests to a given DO are serialised by the platform; no race conditions on the `skills` table or conversation history

---

### 2.4 — Reference Skills

#### `skill-memory` — Rust Durable Object

A Rust DO implementing an MCP server. Backed by its own embedded SQLite database. DO identity: `memory:{user_id}` — one per user, guaranteed isolated.

Exposes tools: `memory_store`, `memory_retrieve`, `memory_list`, `memory_delete`.

Because it is a DO, its state is durable and strongly consistent. It can be extended with semantic search (via embeddings stored in SQLite or routed to Vectorize) in Phase 3.

#### `skill-web-search` — Rust Worker

A stateless Rust Worker wrapping a search API (Brave Search, Tavily, or SearXNG). The API key is stored as a Worker secret binding.

Exposes tools: `web_search(query, max_results?)`.

#### `skill-http-fetch` — Rust Worker

A stateless Rust Worker that fetches and sanitises URL content. The allowlist of permitted domains is read from the calling `AgentDO`'s `prefs` table, passed as an argument on the MCP call, and enforced inside the skill Worker.

Exposes tools: `http_fetch(url)`.

---

### 2.5 — Multi-Agent Patterns with Skills

Skills compose naturally with the multi-agent topology from Phase 1. Each sub-agent DO has its own skill bindings — the orchestrator delegates tasks knowing each specialist only has access to the tools relevant to its role.

**Swarm pattern** — parallel specialised agents:

The orchestrator DO fans out to specialist DOs using concurrent `stub.fetch()` calls. Each specialist runs its own `AgentDO` turn with its own tool set and returns a result. The orchestrator assembles the results and runs a final synthesis turn.

**Pipeline pattern** — sequential skill-enriched stages:

```
IngestDO        EnrichDO         SummaryDO        NotifyDO
(http-fetch)    (web-search)     (no tools)        (email skill)
     │               │               │                  │
     └───────────────┴───────────────┴──────────────────┘
               each DO calls the next via stub.fetch()
```

**Human-in-the-loop** — the DO's single-threaded execution model makes pausing natural. When `pending_tool_calls` contains a call flagged as destructive (e.g. `send_email`, `delete_file`), the DO persists the pending call to the `pending_approvals` table, sends a confirmation request over the WebSocket connection, and returns. The DO hibernates. When the user approves via a subsequent message, the DO reads the pending approval from SQLite, executes it, and resumes the loop.

---

### 2.6 — Updated wrangler.toml (Phase 2)

```toml
name = "edgeclaw"
main = "build/worker/shim.mjs"
compatibility_date = "2026-01-01"

[build]
command = "cargo install -q worker-build && worker-build --release"

# Primary agent actor
[[durable_objects.bindings]]
name = "AGENT_DO"
class_name = "AgentDo"

# Memory skill — separate DO, isolated from agent state
[[durable_objects.bindings]]
name = "MEMORY_SKILL_DO"
class_name = "MemorySkillDo"
script_name = "skill-memory"    # deployed as a separate Worker script

[[migrations]]
tag = "v1"
new_sqlite_classes = ["AgentDo", "MemorySkillDo"]

# Stateless skill Workers — bound via service bindings
[[services]]
binding = "SKILL_WEB_SEARCH"
service  = "skill-web-search"

[[services]]
binding = "SKILL_HTTP_FETCH"
service  = "skill-http-fetch"

# Secrets set via: wrangler secret put <NAME>
# ANTHROPIC_API_KEY
# SEARCH_API_KEY
```

---

### 2.7 — Phase 2 Milestones

| Milestone | Description | Done When |
|---|---|---|
| M2.1 | `mcp-client` connects to a real MCP server and lists tools | Integration test against a local MCP server |
| M2.2 | `mcp-client` invokes a tool and returns a typed result | Tool call round-trip test passes |
| M2.3 | `SkillRegistry` loads from SQLite and dispatches tool calls correctly | Multi-skill dispatch test in Miniflare |
| M2.4 | `skill-memory` deployed as Rust DO and integrated | Agent stores and retrieves facts across turns |
| M2.5 | `skill-web-search` deployed and integrated | Agent answers questions using live search results |
| M2.6 | Dynamic skill registration survives DO eviction | Register skill, evict DO, restart — skill still present |
| M2.7 | Orchestrator DO fans out to two specialist DOs in parallel | Two-agent swarm smoke test passes |
| M2.8 | Human-in-the-loop: DO pauses on destructive tool call and resumes after approval | Approval flow over WebSocket confirmed |
| M2.9 | Full end-to-end with 2+ skills on real Cloudflare deployment | Demo scenario passes against production |

---

## Security Model Summary

| Threat | Phase 1 Mitigation | Phase 2 Mitigation |
|---|---|---|
| Prompt injection via tool output | Max iterations cap; structured result parsing | Same + output length cap and sanitisation in `mcp-client` |
| Runaway agent (e.g. deletes inbox) | No tools in Phase 1 | Human-in-the-loop pause before destructive tool calls |
| Skill escaping its sandbox | N/A | V8 isolate + HTTP-only boundary; shared memory is architecturally impossible |
| Cross-user state leakage | DO identity tied to authenticated user ID | Same; `skill-memory` DO also keyed per user |
| Malicious or hijacked MCP server | N/A | URL allowlist in `AgentDO` prefs; only user-declared skill URLs are used |
| Conversation state tampering | State lives in DO SQLite, not in the client request | Same |
| API key exposure | Stored as Worker secret binding; never written to SQLite | Same; secrets not accessible to skill Workers |
| Race conditions on DO state | Platform serialises all requests to a given DO | Same; tool dispatch inside a single DO turn is sequential |
| Sub-agent acting outside its delegated scope | N/A | Each sub-agent DO has its own skill bindings; orchestrator controls what task it delegates |

---

## Open Questions for Phase 3+

- **Authentication to user-provided MCP servers** — OAuth PKCE flow managed by `AgentDO`? Bearer tokens stored in DO SQLite with encrypted values? This needs a design before dynamic skill registration is exposed to untrusted server URLs.
- **Streaming token delivery** — `AgentDO` already supports WebSocket hibernation. The next step is streaming LLM tokens over the WebSocket as they arrive, rather than waiting for the complete response. Requires SSE-stream parsing in the LLM client.
- **Cloudflare Workflows for long-running tasks** — for agent runs lasting minutes or hours (e.g. deep research), Cloudflare Workflows (built on DOs, up to 25,000 steps, 1GB persisted state, automatic retry) is a better fit than a single DO invocation. `AgentDO` could hand off long tasks to a Workflow and poll for completion.
- **Workers AI fallback** — route privacy-sensitive tasks or cost-overflow to Cloudflare Workers AI (on-device small models). The `base_url` field on `LlmConfig` already supports this; the routing policy needs defining.
- **Messaging frontends** — Telegram webhook is the natural Phase 3 integration. The dispatcher routes `POST /telegram/{user_id}` to `AgentDO.id_from_name("agent:telegram:{user_id}")`. WhatsApp via Twilio API second.
- **Conversation window management** — the SQLite `messages` table makes windowing a simple `ORDER BY id DESC LIMIT N` query. A summarisation strategy for archiving old history before the context window overflows needs to be defined.
- **DO eviction latency** — DOs evict after approximately 10 seconds of inactivity. For latency-sensitive frontends (e.g. voice), keep-alive pings from the messaging layer or pre-warming on incoming webhook receipt may be needed.
- **workers-rs memory leak (issue #722)** — monitor resolution of the known memory leak on DO eviction in `workers-rs` before scaling to production traffic.
