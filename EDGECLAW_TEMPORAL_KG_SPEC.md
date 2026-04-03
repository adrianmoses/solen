# EdgeClaw — Temporal Knowledge Graph & Skill Creation Specification

> Adds a temporal knowledge graph layer over SurrealDB for unified knowledge
> management (skills, preferences, facts, relationships) with drift detection,
> proactive self-healing, and agentic read/write access.
>
> _Depends on: [Agent Improvements](EDGECLAW_AGENT_IMPROVEMENTS_SPEC.md) Phases 6
> (SurrealDB migration) and 9 (Embedding RAG). This spec defines Phases 10-12._
>
> _Companion specs: [Architecture](EDGECLAW_SPEC.md), [Credentials](EDGECLAW_CREDENTIALS_SPEC.md),
> [Agent Improvements](EDGECLAW_AGENT_IMPROVEMENTS_SPEC.md)._

---

## Current State (Post Phase 9)

After the Agent Improvements spec is complete, knowledge lives in three
disconnected stores:

| Store | Model | Temporal? | Relationships? |
|-------|-------|-----------|----------------|
| `memory_fact` | Key-value + tags + embedding | `created_at` only | None |
| `document` | Chunked text + embedding | `created_at` only | None |
| `pref` | Key-value | None | None |

Key limitations:

- **No versioning.** Updating a fact overwrites the previous value. There is no
  history of what changed, when, or why.
- **No relationships.** Facts, preferences, and skills are isolated. There is no
  way to express "skill X depends on tool Y" or "preference A was inferred from
  conversation B."
- **No drift detection.** Stale knowledge looks identical to fresh knowledge.
  The agent cannot distinguish a preference set yesterday from one set six months
  ago.
- **No provenance.** When the consolidation agent writes a fact, there is no
  record of which conversation it was derived from, making audit and rollback
  impossible.

---

## Design Principles

1. **Agent proposes, Rust validates.** The agent writes to the KG through a
   structured tool interface. The Rust layer enforces schema, confidence bounds,
   conflict resolution, and approval gates. The agent never has raw query access.

2. **Relational where flat, graph where connected.** Messages, credentials, and
   scheduled tasks stay as relational tables. Knowledge entities with
   relationships (facts, skills, preferences, dependencies) live in the graph
   layer. SurrealDB supports both in the same database.

3. **Temporal by default.** Every graph edge carries `valid_from`, `valid_to`,
   and `confidence`. Queries filter by temporal validity unless explicitly
   requesting history. This is the mechanism for drift detection.

4. **Confidence is earned, not assigned.** Agent-written knowledge starts at a
   capped confidence. Only human confirmation, repeated corroboration, or
   successful use can increase it. Failed retrievals decrease confidence.

---

## 10. Knowledge Graph Schema

### 10.1 Node Types

```surql
-- Knowledge entity: a fact, concept, or named thing
DEFINE TABLE entity SCHEMAFULL;
DEFINE FIELD user        ON entity TYPE record<user>;
DEFINE FIELD kind        ON entity TYPE string
  ASSERT $value IN [
    "fact",         -- declarative knowledge ("user prefers Rust")
    "preference",   -- user preference ("concise replies")
    "skill",        -- agent-created skill (references SKILL.md on disk)
    "tool",         -- a tool the agent can call
    "api",          -- external API or service
    "concept",      -- abstract concept ("temporal graph database")
    "procedure",    -- multi-step workflow
  ];
DEFINE FIELD name        ON entity TYPE string;
DEFINE FIELD content     ON entity TYPE option<string>;  -- detailed description or value
DEFINE FIELD embedding   ON entity TYPE option<array<float>>;
DEFINE FIELD tags        ON entity TYPE array<string> DEFAULT [];
DEFINE FIELD created_at  ON entity TYPE datetime DEFAULT time::now();
DEFINE FIELD created_by  ON entity TYPE string;  -- "agent:{turn_id}", "human", "consolidation"

DEFINE INDEX idx_entity_user ON entity FIELDS user;
DEFINE INDEX idx_entity_kind ON entity FIELDS user, kind;
DEFINE INDEX idx_entity_name ON entity FIELDS user, name;
DEFINE INDEX idx_entity_embedding ON entity FIELDS embedding MTREE DIMENSION 1024;
```

### 10.2 Edge Types (Relations)

All edges are temporal: they carry `valid_from`, `valid_to`, and `confidence`.

```surql
-- Typed relationship between entities
DEFINE TABLE related_to SCHEMAFULL TYPE RELATION IN entity OUT entity;
DEFINE FIELD predicate   ON related_to TYPE string
  ASSERT $value IN [
    "depends_on",       -- skill depends on tool/API
    "supersedes",       -- new fact replaces old fact
    "derived_from",     -- inferred from another entity
    "contradicts",      -- explicit conflict
    "related_to",       -- general association
    "part_of",          -- entity is part of a larger concept
    "used_by",          -- tool/API used by skill/procedure
  ];
DEFINE FIELD valid_from  ON related_to TYPE datetime DEFAULT time::now();
DEFINE FIELD valid_to    ON related_to TYPE option<datetime>;  -- NULL = still valid
DEFINE FIELD confidence  ON related_to TYPE float DEFAULT 0.7
  ASSERT $value >= 0.0 AND $value <= 1.0;
DEFINE FIELD source      ON related_to TYPE string;  -- "agent:{turn_id}", "human", etc.
DEFINE FIELD reasoning   ON related_to TYPE option<string>;  -- why the agent created this

DEFINE INDEX idx_rel_valid ON related_to FIELDS valid_from, valid_to;
DEFINE INDEX idx_rel_predicate ON related_to FIELDS predicate;
```

### 10.3 Entity Version History

When an entity's content changes, the old version is preserved:

```surql
DEFINE TABLE entity_version SCHEMAFULL;
DEFINE FIELD entity     ON entity_version TYPE record<entity>;
DEFINE FIELD content    ON entity_version TYPE string;
DEFINE FIELD changed_by ON entity_version TYPE string;  -- provenance
DEFINE FIELD changed_at ON entity_version TYPE datetime DEFAULT time::now();
DEFINE FIELD reason     ON entity_version TYPE option<string>;

DEFINE INDEX idx_version_entity ON entity_version FIELDS entity, changed_at;
```

### 10.4 Confidence Decay

Confidence is not static. A Rust-side decay function reduces confidence based
on age and last successful retrieval:

```rust
pub struct ConfidenceConfig {
    /// Half-life in days: confidence halves after this many days without use
    pub half_life_days: f64,       // default: 90.0
    /// Minimum confidence before an edge is considered stale
    pub stale_threshold: f64,      // default: 0.3
    /// Maximum confidence an agent can assign (human can go to 1.0)
    pub agent_ceiling: f64,        // default: 0.7
    /// Confidence boost on successful retrieval (capped at source ceiling)
    pub retrieval_boost: f64,      // default: 0.05
    /// Confidence penalty on failed/contradicted retrieval
    pub contradiction_penalty: f64, // default: 0.2
}

pub fn decayed_confidence(
    base: f64,
    days_since_last_use: f64,
    config: &ConfidenceConfig,
) -> f64 {
    base * (0.5_f64).powf(days_since_last_use / config.half_life_days)
}
```

Decay is computed at **query time**, not stored. The `confidence` field on the
edge is the base confidence at time of last update. This avoids needing a
background job to update every edge.

---

## 11. Agentic KG Access (Knowledge Tools)

### 11.1 Tool Interface

The agent accesses the KG through structured tools. It never writes raw SurQL.
The Rust layer validates every write.

**New built-in tools:**

| Tool | Action | Approval Required |
|------|--------|-------------------|
| `kg_write` | Create/update an entity or edge | No (capped confidence) |
| `kg_query` | Read entities and traverse relationships | No |
| `kg_delete` | Soft-delete an entity or close an edge | Yes |
| `kg_history` | View version history of an entity | No |

### 11.2 `kg_write` — Agent Writes to the Graph

```json
{
  "name": "kg_write",
  "description": "Store or update knowledge in the knowledge graph. Use after learning a new fact, discovering a relationship, or when existing knowledge needs correction.",
  "parameters": {
    "type": "object",
    "properties": {
      "action": {
        "type": "string",
        "enum": ["create_entity", "update_entity", "create_edge", "close_edge"]
      },
      "entity": {
        "type": "object",
        "description": "For create_entity/update_entity",
        "properties": {
          "kind": { "type": "string" },
          "name": { "type": "string" },
          "content": { "type": "string" },
          "tags": { "type": "array", "items": { "type": "string" } }
        }
      },
      "edge": {
        "type": "object",
        "description": "For create_edge/close_edge",
        "properties": {
          "from_name": { "type": "string" },
          "to_name": { "type": "string" },
          "predicate": { "type": "string" },
          "reasoning": { "type": "string" }
        }
      },
      "confidence": {
        "type": "number",
        "description": "Your confidence in this knowledge (0.0-0.7). Be honest."
      },
      "reasoning": {
        "type": "string",
        "description": "Why you are writing this. Used for provenance and audit."
      }
    },
    "required": ["action", "reasoning"]
  }
}
```

### 11.3 Rust Validation Layer

Every `kg_write` call passes through validation before touching the database:

```rust
pub struct KgWriteValidator {
    pub config: ConfidenceConfig,
}

impl KgWriteValidator {
    pub fn validate(&self, call: &KgWriteInput, turn_id: &str) -> Result<ValidatedWrite, KgError> {
        // 1. Cap confidence at agent_ceiling
        let confidence = call.confidence
            .unwrap_or(0.5)
            .min(self.config.agent_ceiling);

        // 2. Schema validation: kind must be in allowed set, predicate must be valid
        self.validate_kind(&call)?;
        self.validate_predicate(&call)?;

        // 3. Name normalization: lowercase, trim, max 128 chars
        let name = normalize_name(&call.entity.as_ref().map(|e| &e.name))?;

        // 4. Content size limit: max 10,000 chars
        self.validate_content_size(&call)?;

        // 5. Conflict detection: check for existing entity with same name + kind
        //    (handled at execution time, not validation)

        Ok(ValidatedWrite {
            confidence,
            source: format!("agent:{turn_id}"),
            // ... normalized fields
        })
    }
}
```

### 11.4 Conflict Resolution

When the agent writes an entity that already exists:

```rust
pub enum ConflictStrategy {
    /// Close the old entity's edges, create a supersedes edge to the new version.
    /// Old entity content is archived to entity_version.
    Supersede,

    /// Merge: update content, keep existing edges, bump confidence if higher.
    Merge,

    /// Reject: return an error telling the agent the entity already exists.
    Reject,
}
```

**Default behavior by kind:**

| Kind | On Conflict |
|------|-------------|
| `fact` | Supersede (old fact → version history, new fact → supersedes edge) |
| `preference` | Supersede (preferences change over time, keep history) |
| `skill` | Reject (skills have their own update mechanism, see §12) |
| `tool` | Merge (tools gain new metadata, don't replace) |
| `api` | Merge |
| `concept` | Merge |
| `procedure` | Supersede |

### 11.5 `kg_query` — Agent Reads from the Graph

```json
{
  "name": "kg_query",
  "description": "Query the knowledge graph. Search by name, kind, tags, or traverse relationships from a known entity.",
  "parameters": {
    "type": "object",
    "properties": {
      "search": {
        "type": "string",
        "description": "Natural language search query (uses embedding similarity)"
      },
      "kind": {
        "type": "string",
        "description": "Filter by entity kind"
      },
      "name": {
        "type": "string",
        "description": "Exact entity name lookup"
      },
      "traverse": {
        "type": "object",
        "description": "Graph traversal from a starting entity",
        "properties": {
          "from_name": { "type": "string" },
          "predicate": { "type": "string", "description": "Edge type to follow" },
          "depth": { "type": "integer", "default": 1, "maximum": 3 }
        }
      },
      "include_stale": {
        "type": "boolean",
        "default": false,
        "description": "Include low-confidence/expired edges"
      }
    }
  }
}
```

**Query execution in Rust:**

The Rust layer translates the structured query into SurQL, applies temporal
filtering (only valid edges by default), computes confidence decay, and returns
results sorted by decayed confidence:

```rust
pub async fn execute_kg_query(
    db: &Surreal<Any>,
    user_id: &str,
    query: &KgQueryInput,
    config: &ConfidenceConfig,
) -> Result<Vec<KgQueryResult>, KgError> {
    let mut results = Vec::new();

    if let Some(search) = &query.search {
        // Semantic search: embed query, vector similarity over entities
        let embedding = embed(search).await?;
        let entities: Vec<Entity> = db.query(
            "SELECT *, vector::similarity::cosine(embedding, $q) AS score
             FROM entity
             WHERE user = $user_id AND embedding != NONE
             ORDER BY score DESC LIMIT 20"
        ).bind(("q", &embedding))
         .bind(("user_id", user_id))
         .await?.take(0)?;

        results.extend(entities.into_iter().map(|e| KgQueryResult::Entity(e)));
    }

    if let Some(traverse) = &query.traverse {
        // Graph traversal with temporal filtering
        let edges: Vec<TraversalResult> = db.query(
            "SELECT
               ->related_to[WHERE predicate = $pred
                 AND valid_to IS NONE
                 AND confidence >= $threshold]
               ->entity.* AS targets
             FROM entity
             WHERE user = $user_id AND name = $from_name"
        ).bind(("pred", &traverse.predicate))
         .bind(("from_name", &traverse.from_name))
         .bind(("threshold", config.stale_threshold))
         .bind(("user_id", user_id))
         .await?.take(0)?;

        // Apply confidence decay to traversal results
        for edge in &mut edges {
            edge.decayed_confidence = decayed_confidence(
                edge.confidence,
                days_since(edge.valid_from),
                config,
            );
        }

        results.extend(edges);
    }

    // Sort by decayed confidence descending
    results.sort_by(|a, b| b.confidence().partial_cmp(&a.confidence()).unwrap());
    Ok(results)
}
```

### 11.6 Retrieval Feedback Loop

When the agent uses a retrieved entity successfully (the turn completes without
error), boost confidence. When the agent encounters a contradiction or failure,
penalize:

```rust
pub async fn record_retrieval_outcome(
    db: &Surreal<Any>,
    entity_id: &str,
    outcome: RetrievalOutcome,
    config: &ConfidenceConfig,
) -> Result<(), KgError> {
    match outcome {
        RetrievalOutcome::Success => {
            // Boost confidence on all active edges to/from this entity
            db.query(
                "UPDATE related_to SET
                   confidence = math::min(confidence + $boost, $ceiling)
                 WHERE (in = $entity OR out = $entity)
                   AND valid_to IS NONE"
            ).bind(("boost", config.retrieval_boost))
             .bind(("ceiling", config.agent_ceiling))
             .bind(("entity", entity_id))
             .await?;
        }
        RetrievalOutcome::Contradiction { reasoning } => {
            // Penalize and flag for review
            db.query(
                "UPDATE related_to SET
                   confidence = math::max(confidence - $penalty, 0.0)
                 WHERE (in = $entity OR out = $entity)
                   AND valid_to IS NONE"
            ).bind(("penalty", config.contradiction_penalty))
             .bind(("entity", entity_id))
             .await?;
        }
    }
    Ok(())
}
```

This feedback loop is **implicit** — the agent doesn't call a tool for it. The
Rust layer in `run_agent_turn()` tracks which entities were retrieved via
`kg_query` during the turn and records outcomes based on whether the turn
completed successfully.

---

## 12. Skill Creation Tool

### 12.1 Overview

A built-in tool that lets the agent create reusable skills from experience.
Unlike hermes-agent's file-only approach, edgeclaw skills are **dual-stored**:
the SKILL.md lives on disk (for human readability and editing), and a
corresponding `entity` node lives in the KG (for relationship tracking, drift
detection, and temporal history).

### 12.2 Tool Schema

```json
{
  "name": "skill_create",
  "description": "Create a new skill from a successful workflow. Call this after completing a complex task (3+ tool calls), overcoming errors, or discovering a non-trivial procedure. The skill will be available for future use.",
  "parameters": {
    "type": "object",
    "properties": {
      "name": {
        "type": "string",
        "description": "Lowercase, hyphens allowed. e.g. 'deploy-rust-service'"
      },
      "description": {
        "type": "string",
        "description": "One-line description of what this skill does"
      },
      "content": {
        "type": "string",
        "description": "Full SKILL.md content including frontmatter"
      },
      "depends_on": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Tool or API names this skill relies on"
      },
      "tags": {
        "type": "array",
        "items": { "type": "string" }
      }
    },
    "required": ["name", "description", "content"]
  }
}
```

Additional tools:

| Tool | Purpose | Approval |
|------|---------|----------|
| `skill_create` | Create a new skill | No |
| `skill_update` | Update SKILL.md content + KG entity | No |
| `skill_patch` | Targeted find-and-replace in SKILL.md | No |
| `skill_delete` | Remove skill from disk and KG | Yes |
| `skill_list` | List available skills with confidence scores | No |

### 12.3 Creation Flow

When the agent calls `skill_create`:

```rust
pub async fn create_skill(
    db: &Surreal<Any>,
    user_id: &str,
    turn_id: &str,
    input: &SkillCreateInput,
    skills_dir: &Path,
    config: &ConfidenceConfig,
) -> Result<SkillCreateResult, SkillError> {
    // 1. Validate
    validate_skill_name(&input.name)?;
    validate_frontmatter(&input.content)?;
    validate_content_size(&input.content, 100_000)?;

    // 2. Write SKILL.md to disk (atomic: temp file + rename)
    let skill_dir = skills_dir.join(&input.name);
    tokio::fs::create_dir_all(&skill_dir).await?;
    atomic_write(&skill_dir.join("SKILL.md"), &input.content).await?;

    // 3. Create entity node in KG
    let entity: Entity = db.query(
        "CREATE entity CONTENT {
           user: $user_id,
           kind: 'skill',
           name: $name,
           content: $description,
           tags: $tags,
           created_by: $source,
         }"
    ).bind(("user_id", user_id))
     .bind(("name", &input.name))
     .bind(("description", &input.description))
     .bind(("tags", &input.tags))
     .bind(("source", format!("agent:{turn_id}")))
     .await?.take(0)?;

    // 4. Create dependency edges
    for dep in &input.depends_on.unwrap_or_default() {
        // Find or create the tool/API entity
        let dep_entity = find_or_create_entity(db, user_id, dep, "tool").await?;

        db.query(
            "RELATE $skill->related_to->$dep CONTENT {
               predicate: 'depends_on',
               confidence: $confidence,
               source: $source,
               reasoning: 'Declared at skill creation time',
             }"
        ).bind(("skill", &entity.id))
         .bind(("dep", &dep_entity.id))
         .bind(("confidence", config.agent_ceiling))
         .bind(("source", format!("agent:{turn_id}")))
         .await?;
    }

    // 5. Generate embedding for semantic discovery
    if let Ok(embedding) = embed(&format!("{} {}", input.name, input.description)).await {
        db.query("UPDATE $entity SET embedding = $emb")
            .bind(("entity", &entity.id))
            .bind(("emb", &embedding))
            .await?;
    }

    Ok(SkillCreateResult {
        name: input.name.clone(),
        entity_id: entity.id.to_string(),
        path: skill_dir,
    })
}
```

### 12.4 Skill Update with Version History

When a skill is updated, the old content is preserved:

```rust
pub async fn update_skill(
    db: &Surreal<Any>,
    user_id: &str,
    turn_id: &str,
    input: &SkillUpdateInput,
    skills_dir: &Path,
) -> Result<(), SkillError> {
    let entity = find_entity(db, user_id, "skill", &input.name).await?;

    // 1. Archive current version
    let current_content = tokio::fs::read_to_string(
        skills_dir.join(&input.name).join("SKILL.md")
    ).await?;

    db.query(
        "CREATE entity_version CONTENT {
           entity: $entity,
           content: $content,
           changed_by: $source,
           reason: $reason,
         }"
    ).bind(("entity", &entity.id))
     .bind(("content", &current_content))
     .bind(("source", format!("agent:{turn_id}")))
     .bind(("reason", &input.reason))
     .await?;

    // 2. Write new content to disk
    atomic_write(
        &skills_dir.join(&input.name).join("SKILL.md"),
        &input.content,
    ).await?;

    // 3. Update entity timestamp
    db.query("UPDATE $entity SET created_at = time::now()")
        .bind(("entity", &entity.id))
        .await?;

    Ok(())
}
```

---

## 13. Drift Detection & Self-Healing

### 13.1 Drift Sources

Knowledge drifts when the world changes but the graph doesn't. Three detection
mechanisms:

| Mechanism | Trigger | Detection |
|-----------|---------|-----------|
| **Confidence decay** | Time passes | Scheduled query for edges below `stale_threshold` |
| **Tool failure correlation** | A tool call fails | Traverse `depends_on` edges from the failing tool to find affected skills |
| **Preference contradiction** | Agent observes behavior contradicting a stored preference | Agent calls `kg_write(action: "create_edge", predicate: "contradicts")` |

### 13.2 Scheduled Drift Scan

A consolidation task (extending §4.4 from Agent Improvements) that runs
periodically and proactively identifies stale knowledge:

```json
{
  "name": "knowledge_drift_scan",
  "cron": "0 4 * * *",
  "payload": {
    "message": "Run a knowledge health check. Query the knowledge graph for: (1) entities with decayed confidence below 0.3, (2) skills whose dependencies have failed recently, (3) preferences that haven't been corroborated in 60+ days. For each issue found, either update the knowledge, flag it for user review, or close stale edges."
  }
}
```

### 13.3 Reactive Drift Detection (Tool Failure Path)

When a tool call fails during a turn, the Rust layer checks if any skills
depend on that tool and flags them:

```rust
pub async fn on_tool_failure(
    db: &Surreal<Any>,
    user_id: &str,
    tool_name: &str,
    error: &str,
) -> Vec<StaleSkillWarning> {
    // Find skills that depend on this tool
    let affected: Vec<SkillEntity> = db.query(
        "SELECT <-related_to[WHERE predicate = 'depends_on' AND valid_to IS NONE]
                <-entity[WHERE kind = 'skill'].* AS skills
         FROM entity
         WHERE user = $user_id AND kind = 'tool' AND name = $tool_name"
    ).bind(("user_id", user_id))
     .bind(("tool_name", tool_name))
     .await
     .unwrap_or_default()
     .take(0)
     .unwrap_or_default();

    // Penalize confidence on the dependency edges
    for skill in &affected {
        let _ = db.query(
            "UPDATE related_to SET confidence = math::max(confidence - 0.2, 0.0)
             WHERE in = $skill AND predicate = 'depends_on' AND valid_to IS NONE
               AND ->entity.name = $tool_name"
        ).bind(("skill", &skill.id))
         .bind(("tool_name", tool_name))
         .await;
    }

    affected.into_iter().map(|s| StaleSkillWarning {
        skill_name: s.name,
        reason: format!("Tool '{}' failed: {}", tool_name, error),
    }).collect()
}
```

The warnings are injected into the agent's next response as context, prompting
it to investigate and update the affected skills.

### 13.4 Preference Drift Detection

The system prompt instructs the agent to watch for preference contradictions:

```
When you notice the user's behavior contradicts a stored preference
(e.g., they repeatedly ask for detailed explanations but their preference
says "concise"), use kg_write to create a "contradicts" edge with your
reasoning. After 3+ contradictions against the same preference, suggest
updating it.
```

**Query for contradicted preferences (used by drift scan):**

```surql
SELECT
  in.name AS preference,
  count() AS contradiction_count,
  array::group(reasoning) AS reasons
FROM related_to
WHERE predicate = "contradicts"
  AND valid_to IS NONE
  AND out.kind = "preference"
  AND out.user = $user_id
GROUP BY in
HAVING count() >= 3;
```

---

## 14. Context Builder Integration

### 14.1 Knowledge-Aware Prompt Assembly

Replace the flat "top-20 memory facts" injection (§4.1) with a graph-aware
context builder that selects knowledge based on relevance, recency, and
confidence:

```rust
pub async fn build_knowledge_context(
    db: &Surreal<Any>,
    user_id: &str,
    user_message: &str,
    config: &ConfidenceConfig,
    max_tokens: usize,  // budget for knowledge section
) -> Result<String, KgError> {
    let mut sections = Vec::new();

    // 1. Active preferences (always included, highest priority)
    let prefs: Vec<Entity> = db.query(
        "SELECT * FROM entity
         WHERE user = $user_id AND kind = 'preference'
         ORDER BY created_at DESC LIMIT 10"
    ).bind(("user_id", user_id)).await?.take(0)?;

    if !prefs.is_empty() {
        sections.push(format_section("Active preferences", &prefs));
    }

    // 2. Semantically relevant entities (embedding search)
    let query_embedding = embed(user_message).await?;
    let relevant: Vec<Entity> = db.query(
        "SELECT *, vector::similarity::cosine(embedding, $q) AS score
         FROM entity
         WHERE user = $user_id
           AND kind NOT IN ['preference']
           AND embedding != NONE
         ORDER BY score DESC
         LIMIT 15"
    ).bind(("q", &query_embedding))
     .bind(("user_id", user_id))
     .await?.take(0)?;

    // 3. Apply confidence decay, filter stale
    let relevant: Vec<_> = relevant.into_iter().filter(|e| {
        // Check edges to this entity for staleness
        let decayed = decayed_confidence(
            e.confidence_max,  // max confidence of active inbound edges
            days_since(e.created_at),
            config,
        );
        decayed >= config.stale_threshold
    }).collect();

    if !relevant.is_empty() {
        sections.push(format_section("Relevant knowledge", &relevant));
    }

    // 4. Stale knowledge warnings (so agent knows what might be wrong)
    let stale: Vec<Entity> = db.query(
        "SELECT * FROM entity
         WHERE user = $user_id
           AND <-related_to[WHERE valid_to IS NONE].confidence < $threshold
         LIMIT 5"
    ).bind(("user_id", user_id))
     .bind(("threshold", config.stale_threshold))
     .await?.take(0)?;

    if !stale.is_empty() {
        sections.push(format_section(
            "Potentially stale knowledge (verify before using)",
            &stale,
        ));
    }

    // 5. Truncate to token budget
    let context = sections.join("\n\n");
    Ok(truncate_to_tokens(&context, max_tokens))
}
```

### 14.2 System Prompt Guidance

Add to the agent's system prompt:

```
## Knowledge Graph

You have access to a temporal knowledge graph that stores facts, preferences,
skills, and their relationships. Knowledge has confidence scores that decay
over time.

- Use `kg_query` to search for relevant knowledge before acting.
- Use `kg_write` to store new knowledge after completing tasks.
- When you notice stored knowledge is wrong or outdated, update it immediately.
- When a tool fails, check if any skills depend on it and update them.
- When user behavior contradicts a stored preference, record the contradiction.

Your writes are capped at 0.7 confidence. Knowledge becomes authoritative
through successful use, not through your assertion.
```

---

## Implementation Order

### Phase 10 — Knowledge Graph Schema + Agent Tools

- Define `entity`, `related_to`, `entity_version` tables in SurrealDB
- Implement `KgWriteValidator` with confidence capping and conflict resolution
- Implement `kg_write`, `kg_query`, `kg_delete`, `kg_history` built-in tools
- Add retrieval feedback loop to `run_agent_turn()`
- Implement confidence decay (query-time computation)
- **Crates touched:** `edgeclaw-server` (new `kg.rs` module)

### Phase 11 — Skill Creation

- Implement `skill_create`, `skill_update`, `skill_patch`, `skill_delete` tools
- Dual storage: SKILL.md on disk + `entity` node in KG with `depends_on` edges
- Version history via `entity_version`
- System prompt guidance for when to create skills
- **Crates touched:** `edgeclaw-server` (new `skills.rs` module)

### Phase 12 — Drift Detection & Self-Healing

- Implement `on_tool_failure()` reactive drift detection
- Implement `knowledge_drift_scan` scheduled task
- Implement preference contradiction detection and accumulation
- Integrate `build_knowledge_context()` into prompt assembly
  (replaces flat memory injection from Phase 4)
- Add stale knowledge warnings to agent context
- **Crates touched:** `edgeclaw-server` (`kg.rs`, `agent.rs`)

---

## Migration from Flat Memory

After Phase 10 is implemented, run a one-time migration that converts existing
`memory_fact` rows into `entity` nodes:

```rust
pub async fn migrate_memory_facts(db: &Surreal<Any>) -> Result<u64, KgError> {
    let facts: Vec<MemoryFact> = db.query(
        "SELECT * FROM memory_fact"
    ).await?.take(0)?;

    let mut count = 0;
    for fact in facts {
        db.query(
            "CREATE entity CONTENT {
               user: $user,
               kind: 'fact',
               name: $key,
               content: $value,
               tags: $tags,
               created_by: 'migration',
               created_at: $created_at,
             }"
        ).bind(("user", &fact.user))
         .bind(("key", &fact.key))
         .bind(("value", &fact.value))
         .bind(("tags", &fact.tags))
         .bind(("created_at", &fact.created_at))
         .await?;
        count += 1;
    }

    Ok(count)
}
```

The `memory_fact` table can be dropped after migration, or kept as a read-only
archive.
