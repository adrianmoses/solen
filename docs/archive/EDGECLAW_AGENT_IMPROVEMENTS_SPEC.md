# EdgeClaw — Agent Improvements Specification

> Improvements to the agent loop, tool execution, multi-agent coordination, and
> retrieval-augmented generation (RAG).
>
> _Companion specs: [Architecture](EDGECLAW_SPEC.md), [Credentials](EDGECLAW_CREDENTIALS_SPEC.md), [CLI](edgeclaw-cli-spec.md)._

---

## Current State

EdgeClaw uses a **tool-use loop** (not ReAct). The agent sends messages + tool
definitions to the Anthropic API, checks `stop_reason`, and returns pending tool
calls to the server layer for external execution. There is no explicit
Thought/Action/Observation text parsing — tool dispatch is driven entirely by the
API's native `tool_use` stop reason.

Key limitations:
- **Agent loop**: Tool execution is fully external (server layer). The loop
  breaks on every `ToolUse` stop, requiring a round-trip through the server for
  each tool call. No streaming, no auto-compaction, no error recovery.
- **Tools**: Skills are MCP HTTP services. No built-in tools (file I/O, code
  execution). No concurrent tool execution.
- **Coordination**: Single agent per turn. No sub-agents or multi-agent swarms.
- **RAG**: `memory_facts` table with key-value storage, accessed only via
  explicit `memory__fetch` tool calls. No automatic context injection, no
  semantic search, no background consolidation.

---

## 1. Agent Loop

### 1.1 Inline Tool Execution

Move safe tool execution into `agent-core` so the loop only breaks out for
destructive tools requiring approval.

**New trait in `agent-core`:**

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> ToolResult;
    fn needs_approval(&self, call: &ToolCall) -> bool;
    fn is_concurrent_safe(&self, call: &ToolCall) -> bool;
}
```

**Modified loop behavior:**

```
User message
  └─► LLM call
       └─► stop_reason:
            ├─ EndTurn → return answer
            ├─ ToolUse →
            │    ├─ partition calls: safe vs. needs_approval
            │    ├─ execute safe calls inline (concurrent where possible)
            │    ├─ if needs_approval calls remain → break out, return pending
            │    └─ if all executed → feed results back, loop
            ├─ MaxTokens → recovery (see §1.3)
            └─ StopSequence → error
```

The `Agent` struct gains an optional `ToolExecutor`:

```rust
pub struct Agent<H: HttpBackend> {
    pub llm: LlmClient<H>,
    pub max_iterations: u32,
    pub tool_executor: Option<Arc<dyn ToolExecutor>>,  // NEW
}
```

When `tool_executor` is `None`, behavior is identical to today (break on every
`ToolUse`). When present, safe tools execute inline and the loop continues.

### 1.2 Concurrent Tool Execution

When multiple tool calls arrive in a single response, partition by concurrency
safety and execute accordingly:

```rust
let (concurrent, serial) = partition_by_concurrency(tool_calls, &executor);

let mut results = Vec::new();

// Run concurrent-safe tools in parallel
let concurrent_results = futures::future::join_all(
    concurrent.into_iter().map(|tc| executor.execute(&tc))
).await;
results.extend(concurrent_results);

// Run serial tools sequentially
for tc in serial {
    results.push(executor.execute(&tc).await);
}
```

Add a `concurrent_safe` field to `ToolDefinition` (default `false`). MCP skills
can declare this via a custom field in their tool schema. Built-in tools declare
it at registration time.

### 1.3 Error Recovery

**`max_tokens` recovery.** Instead of returning an error, continue the
conversation with a system message asking the LLM to continue its response:

```rust
StopReason::MaxTokens => {
    // Append a user message asking to continue
    let continue_msg = Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Continue from where you left off.".into(),
        }],
        created_at: now_epoch(),
    };
    ctx.messages.push(continue_msg.clone());
    new_messages.push(continue_msg);
    // Loop continues to next iteration
}
```

**Prompt-too-long recovery.** When the API returns a `prompt_too_long` error,
trigger auto-compaction (see §1.4) and retry.

### 1.4 Auto-Compaction

When the estimated token count of the context exceeds a configurable threshold
(default: 80% of model context window), compact before the next LLM call.

**Strategy:**

1. Take all messages before a configurable recency window (e.g., keep last 10
   messages intact).
2. Send older messages to the LLM with a summarization prompt:
   `"Summarize this conversation so far, preserving all tool results, decisions made, and pending work."`
3. Replace older messages with a single `CompactBoundary` message containing the
   summary.
4. Continue the loop with the compacted context.

**New type:**

```rust
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
    CompactBoundary { summary: String },  // NEW
}
```

**Token estimation:** Use `content.len() / 4` as a rough heuristic. A more
accurate approach can use `tiktoken-rs` or the API's token counting endpoint
later.

### 1.5 Streaming

Add streaming support so callers can observe partial responses in real time.

**New return type for streaming mode:**

```rust
pub enum AgentEvent {
    TextDelta(String),                    // Partial text chunk
    ToolCallStart(ToolCall),              // Tool call identified
    ToolResult(ToolResult),               // Tool execution completed
    TurnComplete(AgentRunResult),         // Final result
    CompactionStarted,                    // Context compaction in progress
    Error(AgentError),
}
```

The agent exposes a streaming method that returns a channel receiver:

```rust
impl<H: HttpBackend> Agent<H> {
    pub fn run_stream(
        &self,
        ctx: AgentContext,
        user_message: &str,
    ) -> mpsc::Receiver<AgentEvent> { ... }
}
```

This requires the `LlmClient` to support SSE streaming from the Anthropic API
(`stream: true` parameter), parsing `message_delta` and `content_block_delta`
events.

---

## 2. Tool System

### 2.1 Built-in Tools

Register tools that execute in-process (not via MCP HTTP) for core operations
the agent needs without external skill dependencies.

**Initial built-in tools:**

| Tool | Description | Concurrent |
|------|-------------|------------|
| `bash` | Execute shell commands in a sandboxed subprocess | No |
| `file_read` | Read file contents | Yes |
| `file_write` | Write/create files | No |
| `file_edit` | Apply string replacements to files | No |
| `glob` | Find files by pattern | Yes |
| `grep` | Search file contents with regex | Yes |
| `memory_store` | Store a fact (key/value with optional tags) | Yes |
| `memory_fetch` | Retrieve facts by key or tags | Yes |
| `memory_list` | List stored facts for the user | Yes |
| `memory_delete` | Remove a fact by key | No |
| `spawn_agent` | Fork a sub-agent (see §3) | Yes |
| `send_message` | Send message to running agent (see §3) | Yes |
| `stop_agent` | Kill a running agent (see §3) | No |

Memory tools query the `memory_facts` table directly via the database pool —
no MCP round-trip. This makes memory a core capability that works without
external skill containers. The `skill-memory` MCP service can be retired once
built-in memory tools are implemented.

**Implementation:** A new `BuiltinExecutor` that implements `ToolExecutor` and
wraps both built-in tools and the existing `SkillRegistry` for MCP tools:

```rust
pub struct BuiltinExecutor {
    registry: Arc<SkillRegistry>,
    builtins: HashMap<String, Arc<dyn BuiltinTool>>,
}

#[async_trait]
pub trait BuiltinTool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn needs_approval(&self, input: &Value) -> bool;
    fn is_concurrent_safe(&self) -> bool;
    async fn execute(&self, input: Value) -> ToolResult;
}
```

Dispatch checks builtins first, then falls through to `registry.dispatch()`.

### 2.2 Permission Layers

Replace the current binary `is_destructive()` check with a layered permission
system:

```rust
pub enum PermissionCheck {
    Allow,
    Deny(String),
    RequiresApproval(String),  // Reason shown to user
}

pub trait PermissionPolicy: Send + Sync {
    fn check(&self, tool_call: &ToolCall) -> PermissionCheck;
}
```

**Default policy chain** (evaluated in order, first match wins):

1. **Deny list** — Tools explicitly blocked (e.g., `rm -rf /`)
2. **Allow list** — Tools explicitly approved (e.g., all read-only tools)
3. **Pattern matching** — Current destructive pattern check (`delete`, `remove`,
   `send`, `drop` in tool name)
4. **Default** — `RequiresApproval` for unknown tools

Users can configure custom policies via a `permissions` section in prefs.

---

## 3. Multi-Agent Coordination

### 3.1 Sub-Agents

A sub-agent is a new `Agent` instance spawned by the parent via the
`spawn_agent` built-in tool. It runs with its own message history, tool set, and
iteration budget.

**`spawn_agent` tool input schema:**

```json
{
  "task": "string — the task description, sent as the user message",
  "agent_type": "string — optional preset: 'explorer' | 'worker' | 'planner'",
  "tools": ["array — optional tool name whitelist"],
  "model": "string — optional model override",
  "max_iterations": "number — optional, default 10"
}
```

**Agent type presets:**

| Type | Tools | Purpose |
|------|-------|---------|
| `explorer` | `file_read`, `glob`, `grep`, `bash` (read-only) | Research, code exploration |
| `worker` | All built-in + MCP skills | Execute changes |
| `planner` | None (text-only LLM call) | Plan and decompose tasks |

**Execution model:**

- **Sync sub-agents** (default): Parent blocks until child completes. Child's
  final answer is returned as the `spawn_agent` tool result.
- **Async sub-agents** (`"async": true` in input): Child runs in a background
  `tokio::spawn` task. `spawn_agent` returns immediately with an `agent_id`.
  Parent uses `send_message` to communicate and receives completion
  notifications.

**Context isolation:** Sub-agents get:
- Fresh message history (just the task as the first user message)
- Filtered copy of parent's tool definitions
- Shared read access to `SkillRegistry` (same MCP connections)
- Own iteration counter and `CancellationToken`

### 3.2 Agent Registry

Track running agents for the `send_message` and `stop_agent` tools:

```rust
pub struct AgentRegistry {
    agents: DashMap<String, AgentHandle>,
}

pub struct AgentHandle {
    pub id: String,
    pub parent_id: Option<String>,
    pub status: AgentStatus,  // Running, Completed, Failed, Killed
    pub task: String,
    pub tx: mpsc::Sender<String>,  // Send messages to this agent
    pub cancel: CancellationToken,
    pub join: JoinHandle<AgentRunResult>,
}
```

**`send_message` tool:** Sends a follow-up user message to a running async
agent. The agent's loop receives it via the `tx` channel and processes it as a
new turn.

**`stop_agent` tool:** Triggers the agent's `CancellationToken`, waits for
graceful shutdown (5s timeout), then force-kills. Returns the agent's last
result.

### 3.3 Swarm Coordination

For complex tasks, a coordinator agent spawns multiple workers and orchestrates
their collaboration.

**Coordinator mode** is activated by a system prompt directive, not a code path
change. The coordinator agent has access to `spawn_agent`, `send_message`, and
`stop_agent` tools. Workers have domain tools but cannot spawn further agents
(prevents recursive explosion).

**Communication pattern:**

```
Coordinator
  ├─► spawn_agent("research API docs", type=explorer) → agent_1
  ├─► spawn_agent("write tests", type=worker) → agent_2
  │
  │   [agent_1 completes] → result delivered as tool_result
  │
  ├─► send_message(agent_2, "Use these API patterns: ...")
  │
  │   [agent_2 completes] → result delivered as tool_result
  │
  └─► final synthesis
```

**Shared scratchpad (optional):** A temporary directory that all agents in a
swarm can read/write. Useful for passing large artifacts (code files, data)
between agents without stuffing them into messages.

### 3.4 Database Schema Addition

```sql
CREATE TABLE agent_instances (
    id TEXT PRIMARY KEY,
    parent_id TEXT,              -- NULL for root agents
    user_id TEXT NOT NULL,
    status TEXT NOT NULL,        -- 'running', 'completed', 'failed', 'killed'
    agent_type TEXT,
    task TEXT NOT NULL,
    result TEXT,                 -- JSON AgentRunResult on completion
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    FOREIGN KEY (user_id) REFERENCES users(id)
);
```

---

## 4. Retrieval-Augmented Generation (RAG)

### 4.1 Automatic Context Injection

Before each agent turn, inject relevant memory facts into the system prompt
without requiring explicit tool calls.

**Implementation in `run_agent_turn()`:**

```rust
// After loading system_prompt and before building AgentContext:
let facts = sqlx::query!(
    "SELECT key, value FROM memory_facts
     WHERE user_id = ?
     ORDER BY created_at DESC LIMIT 20",
    user_id
).fetch_all(&db).await?;

if !facts.is_empty() {
    let block = facts.iter()
        .map(|f| format!("- {}: {}", f.key, f.value))
        .collect::<Vec<_>>()
        .join("\n");
    system_prompt.push_str(&format!(
        "\n\n## Known facts\n{block}\n"
    ));
}
```

This is a zero-cost improvement — no embeddings, no new infrastructure.

### 4.2 Embedding-Based Retrieval

For scaling beyond the top-20 injection, add semantic search over memory facts
and ingested documents.

**Schema changes:**

```sql
ALTER TABLE memory_facts ADD COLUMN embedding BLOB;

CREATE TABLE documents (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL,
    source TEXT NOT NULL,          -- URL, file path, label
    chunk_index INTEGER NOT NULL,
    content TEXT NOT NULL,
    embedding BLOB,
    token_count INTEGER,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id)
);

CREATE INDEX idx_documents_user ON documents(user_id);
```

**Embedding generation:** Call the Anthropic or OpenAI embeddings API on
insert. Store as `Vec<f32>` serialized to bytes. For offline/local use, consider
`fastembed-rs` (ONNX-based, no API dependency).

**Retrieval flow (per turn):**

1. Embed the user's message.
2. Load all embeddings for the user (memory_facts + documents).
3. Compute cosine similarity in-process (`ndarray` dot product).
4. Take top-K results (default K=10).
5. Inject as a `## Relevant context` section in the system prompt.

For <10k items per user, in-memory cosine search is fast enough (~1ms). If
scale requires it, swap to `sqlite-vss` or an external vector DB later.

### 4.3 Document Ingestion

A new built-in tool `ingest` (or MCP skill) that:

1. Accepts a URL, file path, or raw text.
2. Fetches/reads the content.
3. Strips HTML/extracts text (reuse `skill-http-fetch` logic).
4. Chunks into ~512-token windows with 64-token overlap.
5. Generates embeddings for each chunk.
6. Inserts into `documents` table.

**Tool input schema:**

```json
{
  "source": "string — URL or file path",
  "label": "string — optional human-readable label",
  "raw_text": "string — optional, provide content directly"
}
```

### 4.4 Background Memory Consolidation

Use the existing scheduler to periodically extract durable knowledge from
conversations.

**Scheduled task:**

```json
{
  "name": "memory_consolidation",
  "cron": "0 3 * * *",
  "payload": {
    "message": "Review conversations from the last 24 hours. Extract durable facts about the user (preferences, context, decisions). Store new facts via the memory tools. Update facts that have changed. Remove facts that are contradicted by newer information."
  }
}
```

**Consolidation agent** gets access to:
- `memory_store`, `memory_fetch`, `memory_list`, `memory_delete` built-in tools
- Read-only access to recent messages (injected as context)
- A system prompt tuned for fact extraction (not general assistance)

**Gating (avoid redundant runs):**

```rust
pub struct ConsolidationGate {
    pub min_hours_between_runs: u64,    // default: 24
    pub min_messages_since_last: u64,   // default: 20
}
```

Only trigger consolidation if both thresholds are met. Store
`last_consolidation_at` and `last_consolidation_message_id` in the `prefs`
table.

---

## 5. Agent Soul

### 5.1 Overview

Every agent instance has a **soul** — a user-facing identity that shapes how the
agent communicates and behaves. The soul is separated into two layers:

- **Bones** — Predefined archetype traits. Deterministic, not persisted per user.
  Defines behavioral defaults (tone, verbosity, decision style).
- **Soul** — User-customizable identity (name, personality description). Persisted
  once, loaded on every turn, injected into the system prompt.

This separation means bones can be rebalanced or new archetypes added without
invalidating any stored souls, and users can edit their soul without affecting
structural behavior.

### 5.2 Bones — Behavioral Archetypes

Bones are a predefined set of archetype configurations. Each archetype provides
system prompt fragments that shape the agent's communication style.

```rust
pub struct AgentBones {
    pub archetype: Archetype,
    pub tone: Tone,
    pub verbosity: Verbosity,
    pub decision_style: DecisionStyle,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Archetype {
    Assistant,   // Helpful, balanced, default
    Engineer,    // Terse, code-focused, opinionated
    Researcher,  // Thorough, cites sources, asks clarifying questions
    Operator,    // Action-oriented, minimal explanation, executes fast
    Mentor,      // Patient, explains reasoning, teaches
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Tone {
    Neutral,
    Friendly,
    Direct,
    Formal,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Verbosity {
    Terse,       // Minimal output, just answers
    Balanced,    // Default — explains when useful
    Thorough,    // Detailed explanations, shows reasoning
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum DecisionStyle {
    Cautious,    // Asks before acting, prefers approval
    Balanced,    // Asks for destructive actions, auto-executes safe ones
    Autonomous,  // Executes without asking unless truly dangerous
}
```

Each archetype maps to a system prompt fragment:

```rust
impl Archetype {
    pub fn system_prompt_fragment(&self) -> &'static str {
        match self {
            Self::Assistant => "You are a helpful AI assistant. Be clear and concise.",
            Self::Engineer => "You are a senior engineer. Be direct. Prefer code over explanation. State tradeoffs briefly.",
            Self::Researcher => "You are a thorough researcher. Cite sources. Ask clarifying questions when the request is ambiguous.",
            Self::Operator => "You are an operations agent. Execute tasks efficiently. Explain only when asked.",
            Self::Mentor => "You are a patient mentor. Explain your reasoning. Help the user build understanding.",
        }
    }
}
```

**Default bones** (when no soul is configured):

```rust
impl Default for AgentBones {
    fn default() -> Self {
        Self {
            archetype: Archetype::Assistant,
            tone: Tone::Neutral,
            verbosity: Verbosity::Balanced,
            decision_style: DecisionStyle::Balanced,
        }
    }
}
```

### 5.3 Soul — Persisted Identity

The stored soul is small — just the user-chosen or LLM-generated identity:

```rust
#[derive(Serialize, Deserialize)]
pub struct StoredSoul {
    pub name: String,              // "Atlas", "Kai", etc.
    pub personality: String,       // Free-text personality description
    pub archetype: Archetype,      // Which bones preset to use
    pub tone: Tone,
    pub verbosity: Verbosity,
    pub decision_style: DecisionStyle,
    pub created_at: i64,
}
```

**Persistence:** Stored in the `prefs` table as `key: "soul"`,
`value: JSON(StoredSoul)`. No new table needed.

```sql
-- SQLite (current)
INSERT OR REPLACE INTO prefs (user_id, key, value)
VALUES (?, 'soul', ?);
```

```surql
-- SurrealDB (after migration)
UPSERT pref SET value = $soul
  WHERE user = $user_id AND key = "soul";
```

### 5.4 Soul Creation — Three Paths

All three paths produce the same `StoredSoul` struct. The creation method is a
UX choice, not an architectural difference.

**Path A: SOUL.md file**

User creates a `SOUL.md` file (in the project directory or a config path). The
server parses it on startup or when the file changes.

Format:

```markdown
---
name: Atlas
archetype: engineer
tone: direct
verbosity: terse
decision_style: autonomous
---

A no-nonsense systems engineer who thinks in terms of data flow and failure
modes. Prefers Rust idioms and will push back on over-engineering. Speaks in
short sentences. Uses analogies from distributed systems to explain concepts.
```

Parser:

```rust
pub fn parse_soul_file(content: &str) -> Result<StoredSoul, SoulParseError> {
    // Split on "---" frontmatter delimiters
    // Parse YAML frontmatter for structured fields
    // Everything after second "---" is the personality free-text
}
```

**Path B: User input via API**

User provides fields directly through the REST API:

```
POST /soul
{
  "name": "Atlas",
  "archetype": "engineer",
  "tone": "direct",
  "verbosity": "terse",
  "decision_style": "autonomous",
  "personality": "A no-nonsense systems engineer..."
}
```

Partial updates supported — omitted fields keep defaults:

```
PATCH /soul
{
  "tone": "friendly"
}
```

**Path C: LLM generation**

User provides minimal input (optionally just an archetype), and the agent
generates the rest:

```
POST /soul/generate
{
  "archetype": "engineer",
  "hints": "likes Rust, dislikes over-abstraction"
}
```

The server sends a one-shot LLM call with a generation prompt:

```rust
const SOUL_GEN_PROMPT: &str = r#"
Generate a name and personality for an AI agent with these traits:
- Archetype: {archetype}
- Tone: {tone}
- User hints: {hints}

Respond with JSON:
{
  "name": "a short, memorable name",
  "personality": "2-3 sentences describing the agent's personality, communication style, and quirks"
}
"#;
```

This is a single LLM call, not a conversation. The result is parsed and stored.

### 5.5 System Prompt Assembly

On each agent turn, the soul is assembled into the system prompt in
`run_agent_turn()`:

```rust
fn build_system_prompt(base: &str, soul: Option<&StoredSoul>) -> String {
    let mut prompt = String::new();

    match soul {
        Some(soul) => {
            // Bones-derived behavioral instruction
            let bones = AgentBones {
                archetype: soul.archetype,
                tone: soul.tone,
                verbosity: soul.verbosity,
                decision_style: soul.decision_style,
            };
            prompt.push_str(bones.archetype.system_prompt_fragment());
            prompt.push_str(&format!(
                "\n\nYour name is {}.\n{}\n",
                soul.name, soul.personality
            ));

            // Tone and verbosity modifiers
            prompt.push_str(&bones.tone.modifier());
            prompt.push_str(&bones.verbosity.modifier());
        }
        None => {
            prompt.push_str(AgentBones::default()
                .archetype.system_prompt_fragment());
        }
    }

    // Append user's custom system prompt (from prefs)
    if !base.is_empty() {
        prompt.push_str(&format!("\n\n{base}"));
    }

    prompt
}
```

**Priority order** (later overrides earlier):

1. Archetype fragment (bones)
2. Name + personality (soul)
3. Tone/verbosity modifiers (bones)
4. User's custom system prompt (prefs `key: "system_prompt"`)
5. Skill context (SKILL.md injections)
6. Memory facts (§4.1)

### 5.6 REST API Endpoints

```
GET    /soul              — Get current soul (returns StoredSoul or 404)
POST   /soul              — Create/replace soul from user input (Path B)
PATCH  /soul              — Partial update (change tone without resetting name)
DELETE /soul              — Remove soul, revert to defaults
POST   /soul/generate     — LLM-generate soul from archetype + hints (Path C)
GET    /soul/archetypes   — List available archetypes with descriptions
```

### 5.7 SOUL.md File Watching (Optional)

For users who prefer file-based configuration, watch for `SOUL.md` in a
configured path:

```rust
// On server startup and file change:
if let Ok(content) = std::fs::read_to_string(&soul_path) {
    let soul = parse_soul_file(&content)?;
    save_soul(&db, &user_id, &soul).await?;
}
```

This can use `notify` crate for filesystem watching, or simply re-read on each
agent turn (the file is small, the cost is negligible).

---

## 6. SurrealDB Migration

### 6.1 Motivation

SQLite serves the current schema well, but the improvements in this spec
introduce requirements that push against its strengths:

- **Vector search (§4.2)** requires `sqlite-vss` (C extension, build complexity)
  or in-memory cosine search (doesn't scale). SurrealDB has native vector
  functions.
- **Agent hierarchy (§3)** requires recursive CTEs to query parent/child
  relationships. SurrealDB's graph model handles this natively.
- **Memory facts with tags (§4)** are currently a serialized TEXT column.
  SurrealDB supports native arrays and set operations.
- **Flexible schema evolution** — adding fields to agent_instances, documents,
  or memory_facts doesn't require ALTER TABLE migrations.

### 6.2 Dependency Change

Replace `sqlx` with the `surrealdb` crate in `edgeclaw-server`:

```toml
# Remove
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite"] }

# Add
surrealdb = { version = "2", features = ["kv-surrealkv"] }
```

**Embedded mode (`kv-surrealkv`)** avoids running a separate SurrealDB server
process. Data persists to a local directory (like SQLite's single file). This
preserves the current zero-infrastructure deployment model. If multi-node is
needed later, switch to client mode connecting to a SurrealDB server.

### 6.3 Schema Design

SurrealDB uses a define-then-use model. Record IDs are typed
(`table:identifier`), and relations are first-class graph edges.

**Users:**

```surql
DEFINE TABLE user SCHEMAFULL;
DEFINE FIELD created_at ON user TYPE datetime DEFAULT time::now();
```

Records created as `user:adrian`, `user:default`, etc.

**Messages:**

```surql
DEFINE TABLE message SCHEMAFULL;
DEFINE FIELD user      ON message TYPE record<user>;
DEFINE FIELD role      ON message TYPE string ASSERT $value IN ["user", "assistant"];
DEFINE FIELD content   ON message TYPE array<object>;  -- Vec<ContentBlock>
DEFINE FIELD created_at ON message TYPE datetime DEFAULT time::now();

DEFINE INDEX idx_message_user_time ON message FIELDS user, created_at;
```

Query last 50 messages:

```surql
SELECT * FROM message
  WHERE user = $user_id
  ORDER BY created_at DESC
  LIMIT 50;
```

**Skills:**

```surql
DEFINE TABLE skill SCHEMAFULL;
DEFINE FIELD user           ON skill TYPE record<user>;
DEFINE FIELD name           ON skill TYPE string;
DEFINE FIELD url            ON skill TYPE string;
DEFINE FIELD tools          ON skill TYPE array<object>;  -- Vec<ToolDefinition>
DEFINE FIELD skill_context  ON skill TYPE option<string>;
DEFINE FIELD auth_header    ON skill TYPE option<object>;  -- { name, value }
DEFINE FIELD session_id     ON skill TYPE option<string>;
DEFINE FIELD added_at       ON skill TYPE datetime DEFAULT time::now();

DEFINE INDEX idx_skill_user_name ON skill FIELDS user, name UNIQUE;
```

**Credentials:**

```surql
DEFINE TABLE credential SCHEMAFULL;
DEFINE FIELD user              ON credential TYPE record<user>;
DEFINE FIELD skill_name        ON credential TYPE string;
DEFINE FIELD provider          ON credential TYPE string;
DEFINE FIELD credential_type   ON credential TYPE string
  ASSERT $value IN ["oauth", "service_account"];
DEFINE FIELD access_token_enc  ON credential TYPE bytes;
DEFINE FIELD refresh_token_enc ON credential TYPE option<bytes>;
DEFINE FIELD metadata_enc      ON credential TYPE option<bytes>;
DEFINE FIELD expires_at        ON credential TYPE option<datetime>;
DEFINE FIELD scopes            ON credential TYPE string;
DEFINE FIELD user_salt         ON credential TYPE bytes;
DEFINE FIELD created_at        ON credential TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at        ON credential TYPE datetime DEFAULT time::now();

DEFINE INDEX idx_cred_unique ON credential
  FIELDS user, skill_name, provider UNIQUE;
```

**Scheduled Tasks:**

```surql
DEFINE TABLE scheduled_task SCHEMAFULL;
DEFINE FIELD user     ON scheduled_task TYPE record<user>;
DEFINE FIELD name     ON scheduled_task TYPE string;
DEFINE FIELD cron     ON scheduled_task TYPE option<string>;
DEFINE FIELD run_at   ON scheduled_task TYPE option<datetime>;
DEFINE FIELD payload  ON scheduled_task TYPE object;
DEFINE FIELD last_run ON scheduled_task TYPE option<datetime>;
DEFINE FIELD enabled  ON scheduled_task TYPE bool DEFAULT true;

DEFINE INDEX idx_task_runnable ON scheduled_task
  FIELDS run_at WHERE enabled = true;
```

**Pending Approvals:**

```surql
DEFINE TABLE pending_approval SCHEMAFULL;
DEFINE FIELD user       ON pending_approval TYPE record<user>;
DEFINE FIELD tool_call  ON pending_approval TYPE object;  -- ToolCall JSON
DEFINE FIELD created_at ON pending_approval TYPE datetime DEFAULT time::now();
```

**Preferences:**

```surql
DEFINE TABLE pref SCHEMAFULL;
DEFINE FIELD user  ON pref TYPE record<user>;
DEFINE FIELD key   ON pref TYPE string;
DEFINE FIELD value ON pref TYPE string;

DEFINE INDEX idx_pref_unique ON pref FIELDS user, key UNIQUE;
```

### 6.4 Memory Facts — Native Tags + Vector Search

```surql
DEFINE TABLE memory_fact SCHEMAFULL;
DEFINE FIELD user       ON memory_fact TYPE record<user>;
DEFINE FIELD key        ON memory_fact TYPE string;
DEFINE FIELD value      ON memory_fact TYPE string;
DEFINE FIELD tags       ON memory_fact TYPE array<string> DEFAULT [];
DEFINE FIELD embedding  ON memory_fact TYPE option<array<float>>;
DEFINE FIELD created_at ON memory_fact TYPE datetime DEFAULT time::now();

DEFINE INDEX idx_fact_user_key ON memory_fact FIELDS user, key UNIQUE;
DEFINE INDEX idx_fact_embedding ON memory_fact FIELDS embedding MTREE DIMENSION 1024;
```

**Tag queries (native array operations):**

```surql
-- Facts matching any of these tags
SELECT * FROM memory_fact
  WHERE user = $user_id
    AND tags CONTAINSANY ["preference", "context"];

-- Facts matching all of these tags
SELECT * FROM memory_fact
  WHERE user = $user_id
    AND tags CONTAINSALL ["project", "edgeclaw"];
```

**Semantic search (native vector similarity):**

```surql
SELECT *, vector::similarity::cosine(embedding, $query_embedding) AS score
  FROM memory_fact
  WHERE user = $user_id
    AND embedding != NONE
  ORDER BY score DESC
  LIMIT 10;
```

No `sqlite-vss` extension, no in-memory cosine computation, no BLOB
serialization. The MTREE index accelerates the search.

### 6.5 Documents — Chunked Ingestion with Vector Search

```surql
DEFINE TABLE document SCHEMAFULL;
DEFINE FIELD user        ON document TYPE record<user>;
DEFINE FIELD source      ON document TYPE string;
DEFINE FIELD label       ON document TYPE option<string>;
DEFINE FIELD chunk_index ON document TYPE int;
DEFINE FIELD content     ON document TYPE string;
DEFINE FIELD embedding   ON document TYPE option<array<float>>;
DEFINE FIELD token_count ON document TYPE option<int>;
DEFINE FIELD created_at  ON document TYPE datetime DEFAULT time::now();

DEFINE INDEX idx_doc_user ON document FIELDS user;
DEFINE INDEX idx_doc_source ON document FIELDS user, source, chunk_index UNIQUE;
DEFINE INDEX idx_doc_embedding ON document FIELDS embedding MTREE DIMENSION 1024;
```

**Unified retrieval across memory + documents:**

```surql
-- Search both tables, merge results by score
(SELECT "memory_fact" AS type, key AS title, value AS content,
        vector::similarity::cosine(embedding, $q) AS score
   FROM memory_fact
   WHERE user = $user_id AND embedding != NONE)
UNION
(SELECT "document" AS type, source AS title, content,
        vector::similarity::cosine(embedding, $q) AS score
   FROM document
   WHERE user = $user_id AND embedding != NONE)
ORDER BY score DESC
LIMIT 10;
```

### 6.6 Agent Instances — Graph Relations

```surql
DEFINE TABLE agent_instance SCHEMAFULL;
DEFINE FIELD user         ON agent_instance TYPE record<user>;
DEFINE FIELD status       ON agent_instance TYPE string
  ASSERT $value IN ["running", "completed", "failed", "killed"];
DEFINE FIELD agent_type   ON agent_instance TYPE option<string>;
DEFINE FIELD task         ON agent_instance TYPE string;
DEFINE FIELD result       ON agent_instance TYPE option<object>;
DEFINE FIELD created_at   ON agent_instance TYPE datetime DEFAULT time::now();
DEFINE FIELD completed_at ON agent_instance TYPE option<datetime>;

-- Graph edge: parent spawned child
DEFINE TABLE spawned SCHEMAFULL TYPE RELATION IN agent_instance OUT agent_instance;
DEFINE FIELD created_at ON spawned TYPE datetime DEFAULT time::now();
```

**Spawning a sub-agent:**

```surql
-- Create child
LET $child = CREATE agent_instance CONTENT {
    user: $user_id,
    status: "running",
    agent_type: "explorer",
    task: "Research the API docs",
};

-- Create edge
RELATE $parent->spawned->$child;
```

**Query all descendants of a coordinator (graph traversal):**

```surql
-- Direct children
SELECT ->spawned->agent_instance.* FROM $coordinator;

-- All descendants (recursive)
SELECT ->spawned->agent_instance->spawned->agent_instance.* FROM $coordinator;
```

**Query the full agent tree for a user:**

```surql
SELECT *, ->spawned->agent_instance AS children
  FROM agent_instance
  WHERE user = $user_id
    AND <-spawned IS NONE  -- root agents only (no parent)
  FETCH children;
```

### 6.7 Rust Integration

**Connection setup (embedded mode):**

```rust
use surrealdb::Surreal;
use surrealdb::engine::local::SurrealKV;

let db = Surreal::new::<SurrealKV>("data/edgeclaw.db").await?;
db.use_ns("edgeclaw").use_db("main").await?;
```

**Query pattern (replaces sqlx::query!):**

```rust
// sqlx (before):
let facts = sqlx::query!(
    "SELECT key, value FROM memory_facts WHERE user_id = ? LIMIT 20",
    user_id
).fetch_all(&pool).await?;

// surrealdb (after):
let facts: Vec<MemoryFact> = db.query(
    "SELECT * FROM memory_fact WHERE user = $user_id LIMIT 20"
).bind(("user_id", thing(&format!("user:{user_id}"))?))
 .await?
 .take(0)?;
```

**Trade-off: no compile-time query checking.** `sqlx::query!` validates SQL at
compile time against the database schema. The `surrealdb` crate uses runtime
query strings. Mitigate by:

1. Centralizing all queries in a `db.rs` module (not scattered across handlers).
2. Integration tests that run each query against a test database.
3. Defining `#[derive(Serialize, Deserialize)]` types for every table to catch
   deserialization mismatches early.

### 6.8 Migration Strategy

**Do not migrate everything at once.** Follow the phased approach:

| Phase | Database | Rationale |
|-------|----------|-----------|
| 1-2 (Agent loop, built-in tools) | SQLite | No schema changes. Pure `agent-core` work. |
| 3 (Compaction, streaming) | SQLite | No schema changes. Pure `agent-core` work. |
| 4 (Memory injection) | SQLite | Simple query, no new tables. Ship the quick win. |
| 5 (Agent soul) | SQLite | Just a pref row. No new tables. |
| **6 (Migration window)** | **SQLite → SurrealDB** | **Migrate existing tables. One-time data export/import.** |
| 7-8 (Sub-agents, swarms) | SurrealDB | Graph edges for agent hierarchy. |
| 9 (Embedding RAG) | SurrealDB | Native vector search, MTREE index. |

**Data migration script:** Export SQLite rows as JSON, transform to SurrealQL
`INSERT` statements. The `memory_facts.tags` column (currently TEXT) becomes a
native `array<string>` — split on comma during migration.

---

## Implementation Order

Each phase builds on the previous. Phases 1-2 are prerequisites for 3-4.

### Phase 1 — Agent Loop (§1.1, §1.2, §1.3)

- Add `ToolExecutor` trait to `agent-core`
- Implement inline execution with concurrency partitioning
- Add `max_tokens` recovery
- **Crates touched:** `agent-core`

### Phase 2 — Built-in Tools + Permissions (§2.1, §2.2)

- Implement `BuiltinExecutor` wrapping `SkillRegistry`
- Add `bash`, `file_read`, `file_write`, `file_edit`, `glob`, `grep`
- Replace `is_destructive()` with permission policy chain
- **Crates touched:** `agent-core`, `edgeclaw-server`

### Phase 3 — Auto-Compaction + Streaming (§1.4, §1.5)

- Add `CompactBoundary` content block
- Implement token estimation and summarization
- Add SSE streaming in `LlmClient`
- Expose `AgentEvent` stream
- **Crates touched:** `agent-core`

### Phase 4 — Automatic Memory Injection (§4.1)

- Inject top-20 memory facts into system prompt per turn
- Zero new dependencies
- **Crates touched:** `edgeclaw-server`

### Phase 5 — Agent Soul (§5)

- Define `AgentBones` archetypes and `StoredSoul` struct
- Implement SOUL.md parser (frontmatter + free-text personality)
- Add `POST /soul`, `PATCH /soul`, `GET /soul` REST endpoints
- Add `POST /soul/generate` LLM generation endpoint
- Integrate soul into system prompt assembly in `run_agent_turn()`
- **Crates touched:** `edgeclaw-server`

### Phase 6 — SurrealDB Migration (§6)

- Replace `sqlx` with `surrealdb` crate (embedded SurrealKV mode)
- Migrate existing 8 tables to SurrealQL schema definitions
- Centralize all queries in `db.rs` module
- Write data migration script (SQLite JSON export → SurrealQL INSERT)
- Integration tests for all queries against test database
- **Crates touched:** `edgeclaw-server`

### Phase 7 — Sub-Agents (§3.1, §3.2)

- Implement `spawn_agent`, `send_message`, `stop_agent` built-in tools
- Add `AgentRegistry` for tracking running agents
- Add `agent_instance` table with `spawned` graph edges
- **Crates touched:** `agent-core`, `edgeclaw-server`

### Phase 8 — Swarm Coordination (§3.3)

- Coordinator system prompt and agent type presets
- Shared scratchpad directory
- Recursive spawn prevention
- **Crates touched:** `edgeclaw-server`

### Phase 9 — Embedding RAG (§4.2, §4.3, §4.4)

- Add MTREE-indexed embedding fields to `memory_fact`
- Add `document` table with vector search
- Implement unified retrieval query across both tables
- Add `ingest` tool
- Add consolidation scheduled task and gating
- **Crates touched:** `edgeclaw-server`, possibly new `edgeclaw-rag` crate
