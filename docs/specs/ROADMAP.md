# Roadmap

<!-- status: inferred -->
| Field | Value |
|---|---|
| status | inferred |
| created | 2026-04-17 |
| inferred-from | `ROADMAP.md` (pre-shipspec, dated 2026-04-14), `README.md`, `crates/*/src/**`, `skills/*/`, `crates/edgeclaw-server/migrations/0001_initial.sql`..`0005_souls.sql` |

## Features

| ID | Feature | Status | Spec |
|---|---|---|---|
| 001 | ReAct agent loop (`agent-core`) with `HttpBackend` abstraction, `max_iterations`, `MaxTokens` continuation handling | implemented | — |
| 002 | Built-in tools: `bash`, `file_read`, `file_write`, `file_edit`, `glob`, `grep` | implemented | — |
| 003 | Four-layer `PolicyChain` permissions (deny / allow / destructive-pattern / default-requires-approval) | implemented | — |
| 004 | Anthropic Messages API client (`LlmClient` + `ReqwestBackend`) | implemented | — |
| 005 | MCP client (`mcp-client`, JSON-RPC 2.0: initialize, list_tools, call_tool) | implemented | — |
| 006 | `skill-registry`: namespaced `skill:tool` routing implementing `ToolExecutor` | implemented | — |
| 007 | axum HTTP server with `/message`, `/history`, `/skills*`, `/approve*`, `/tasks*`, `/admin/skills/status`, `/health` | implemented | — |
| 008 | WebSocket session layer with typed `ServerMessage`/`ClientMessage`, `oneshot`-based tool-approval flow, 5-minute auto-deny | implemented | — |
| 009 | SQLite persistence (messages, skills, credentials, scheduled_tasks, pending_approvals, prefs, memory_facts, souls) via sqlx migrations 0001–0005 | implemented | — |
| 010 | Scheduler: 10-second poll loop, one-shot and cron tasks, `AutoApprove` execution | implemented | — |
| 011 | OAuth 2.0 + PKCE flow (`oauth.rs`); GitHub and Google providers auto-loaded from env | implemented | — |
| 012 | Encrypted credential storage (AES-256-GCM + HKDF; `credential-store` crate) | implemented | — |
| 013 | External MCP integration: Google Workspace via `workspace-mcp` container (Gmail, Calendar, Drive) | implemented | — |
| 014 | External MCP integration: GitHub via user-registered MCP URL (`SKILL_GITHUB_URL` / `SKILL_GITHUB_AUTH_TOKEN`) | implemented | — |
| 015 | Docker Compose deployment (`agent`, `workspace-mcp`, `caddy`), `docker-compose.prod.yml` override pulling `ghcr.io/adrianmoses/edgeclaw:latest`, `deploy/Caddyfile`, `deploy/edgeclaw.service` | implemented | — |
| 016 | Phase 1.1 — `edgeclaw serve` foreground with `--host` / `--port` (`127.0.0.1:7100` default) | implemented | [`GAP.md`](GAP.md) |
| 017 | Phase 1.2 — `edgeclaw chat` inline raw-mode TUI: WS connect, auto-spawn local server, inline approval prompts, coloured `agent>` / `error>` / `[tool]` output, Ctrl-C / Ctrl-D graceful exit | implemented | [`GAP.md`](GAP.md) |
| 018 | Phase 1.3 — `edgeclaw config` (first-run wizard, `config show`/`edit`, `set model`, `set personality`, `set approval`, `set tools`, `connector add/list/remove/test`). TOML at `~/.config/edgeclaw/config.toml` with env-var overrides and atomic writes | in-progress | [`GAP.md`](GAP.md) |
| 019 | Phase 2 — Agent Soul: archetype/tone/verbosity/decision-style enums, `compose_system_prompt`, `SOUL.md` parse/emit, `souls` table (migration 0005), REST `GET/POST/PATCH /soul` + `POST /soul/generate`, CLI `edgeclaw soul show|set|edit|generate|import|export` | implemented | — |
| 020 | Phase 1.1 extras — `--daemonize`, `--pid-file`, `serve status`/`serve stop`/`serve restart`, optional TLS (`--tls-cert` / `--tls-key`) | planned | [`GAP.md`](GAP.md) |
| 021 | Phase 1.2 extras — pipe mode (`--no-tui`), named sessions (`--session-id`), agent selection (`--agent`), multiline input (Ctrl-J), approve-all-in-session (`[a]`) | planned | [`GAP.md`](GAP.md) |
| 022 | Phase 1.3 extras — connector runtime (Telegram / Discord / Slack listeners on the server side; CLI side today only stores config) | planned | [`GAP.md`](GAP.md) |
| 023 | Phase 3.1 — `spawn_agent` built-in tool with type presets (`explorer`, `worker`, `planner`), fresh history, filtered tools, own iteration budget and cancellation token | planned | — |
| 024 | Phase 3.2 — sync and async sub-agent execution models; `send_message` and completion notifications | planned | — |
| 025 | Phase 3.3 — agent registry, persisted agent state in SQLite for crash recovery, `stop_agent` tool | planned | — |
| 026 | Phase 3.4 — swarm coordination (coordinator spawns workers; workers can't recurse); optional shared scratchpad directory | planned | — |
| 027 | First-party `skill-github` implementation (currently `SKILL.md` only) | planned | `skills/skill-github/SKILL.md` |
| 028 | First-party `skill-gmail` implementation (currently `SKILL.md` only) | planned | `skills/skill-gmail/SKILL.md` |
| 029 | First-party `skill-google-calendar` implementation (currently `SKILL.md` only; Google Workspace MCP covers today) | planned | `skills/skill-google-calendar/SKILL.md` |
| 030 | Embeddings / RAG over memory and documents, automatic context injection, background consolidation | deprecated | `docs/archive/EDGECLAW_AGENT_IMPROVEMENTS_SPEC.md` |
| 031 | SurrealDB migration replacing SQLite + sqlx, enabling native graph + vector queries | deprecated | `docs/archive/EDGECLAW_SPEC.md` |
| 032 | Temporal knowledge graph with versioned entities, confidence decay, drift detection, self-healing | deprecated | `docs/archive/EDGECLAW_TEMPORAL_KG_SPEC.md` |
| 033 | Auto-compaction and SSE streaming from agent to client | deprecated | — |

## Status Values

- `planned` — not yet started
- `in-progress` — spec written, implementation underway
- `implemented` — decision record complete
- `deprecated` — removed from product

Note: `deprecated` here is used (per the skill template) for explicitly *parked* features — the prior `ROADMAP.md` labelled them "Deferred." If a distinct `deferred` status is preferred, update these entries.

## Revision History

| Date | Change |
|---|---|
| 2026-04-17 | Initial roadmap inferred by shipspec-audit skill from codebase, `README.md`, and existing top-level `ROADMAP.md` (dated 2026-04-14). |
| 2026-04-17 | Removed lingering Cloudflare Workers skill crates (`skill-memory`, `skill-web-search`, `skill-http-fetch`, `skill-schedule`, `mcp-server-util`) and all `worker`/`wasm32` scaffolding. Skills are now entirely external MCP servers. |
