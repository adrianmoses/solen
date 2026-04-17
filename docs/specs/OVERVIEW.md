# Overview

<!-- status: inferred -->
| Field | Value |
|---|---|
| status | accepted |
| created | 2026-04-17 |
| inferred-from | `README.md`, `CLAUDE.md`, `Cargo.toml`, `crates/agent-core/src/{lib,agent,types,soul,permissions}.rs`, `crates/edgeclaw-server/src/{server,session,scheduler,oauth,startup}.rs`, `crates/edgeclaw-server/migrations/*.sql`, `crates/edgeclaw-cli/src/{main,chat,config,soul}.rs`, `skills/skill-{github,gmail,google-calendar}/SKILL.md`, `docker-compose.yml`, `deploy/Caddyfile`, `deploy/edgeclaw.service` |

## Product Summary

EdgeClaw is a self-hosted, single-tenant personal AI agent runtime. A Rust workspace pairs a pure-Rust ReAct loop (`agent-core`) with an axum + sqlx host (`edgeclaw-server`), an end-user CLI (`edgeclaw`), and a set of MCP-over-HTTP skills. Users send messages through a WebSocket or `POST /message`; the agent drives the Anthropic Messages API, executes built-in tools (`bash`, `file_read`, `file_write`, `file_edit`, `glob`, `grep`) or MCP skill tools, and — when a tool is flagged by the permission policy — pauses for human approval before continuing. State (messages, skills, credentials, scheduled tasks, preferences, souls) lives in local SQLite. A per-user "soul" (archetype + tone + verbosity + decision style + free-text personality) is injected into the system prompt every turn. Deployment is Docker Compose on a VPS behind Caddy.

## Target Consumer

Inferred from the surface area (local-first CLI, `127.0.0.1` binding by default, single-user config in `~/.config/edgeclaw/config.toml`, VPS deployment docs, no multi-tenant auth): a **technical individual operator** running their own agent — a developer, power user, or hobbyist who wants a scriptable AI assistant they control, rather than a SaaS or team tool. The WebSocket approval loop and inline TUI suggest interactive desk use; the scheduler and OAuth flow (GitHub, Google) suggest the same user also wants the agent to act autonomously on their behalf against their accounts.

The product is intended for external users beyond the author. `ghcr.io/adrianmoses/edgeclaw:latest` in `docker-compose.prod.yml` suggests distribution to at least some outside users.

## Job To Be Done

Give one person a private, always-available agent that can:

1. Converse over a persistent SQLite-backed history.
2. Run a curated set of local tools (shell, filesystem, search/grep) with layered permission gating.
3. Call out to external MCP skills (Google Workspace for Gmail/Calendar/Drive, GitHub MCP, or any user-registered MCP URL) with encrypted per-user OAuth credentials.
4. Act on a schedule (cron or one-shot) without the user present.
5. Have a stable identity and voice (the "soul") that the user can shape via CLI or `SOUL.md`.

## Non-Goals

Inferred from what the code does *not* implement:

- **No multi-tenant SaaS.** `user_id` is a string but there is no authentication, session token, or tenant isolation layer — the server is a trusted-network deployment.
- **No streaming responses** to clients. Turns return whole `agent_response` messages over WS/HTTP. (Deprecated in `docs/specs/ROADMAP.md`.)
- **No embeddings / RAG / vector search.** `memory_facts` is a KV-with-tags table, not a semantic index. (Listed as deferred.)
- **No temporal knowledge graph, no SurrealDB.** Archived specs exist in `docs/archive/` but are explicitly parked.
- **No sub-agent orchestration yet.** Features 023–026 in `docs/specs/ROADMAP.md`; no `spawn_agent` tool in `crates/agent-core/src/builtins/`.
- **No web/native GUI.** Interaction is CLI, WebSocket, or raw HTTP only.

## Tech Stack

**Language & runtime**
- Rust (stable, edition 2021) across the workspace.
- Cargo workspace with 6 crates under `crates/`. The `skills/` directory holds only `SKILL.md` prompt-context files for external MCP servers — no in-repo skill crates.

**Server (`edgeclaw-server`)**
- `axum 0.8` with `ws` feature, `tokio 1` full features.
- `sqlx 0.8` with `sqlite` + `migrate` runtime-tokio.
- `tower-http` (cors, trace), `tracing` / `tracing-subscriber`.
- `reqwest 0.12` for outbound Anthropic + MCP calls.
- `jsonwebtoken 9` (present — OAuth/credential use).
- `cron 0.15`, `chrono 0.4` for the scheduler.
- `base64`, `sha2`, `rand` for PKCE and credential encryption.

**Agent core (`agent-core`)**
- Pure Rust with no server framework dependency.
- `native` feature gates `reqwest`, `tokio`, `glob`, `regex`, `walkdir`.
- `serde` / `serde_json` / `serde_yaml` for message and soul serialization.

**CLI (`edgeclaw-cli`)**
- `clap 4` (derive), `tokio-tungstenite 0.28`, `crossterm 0.28` with `event-stream`.
- `inquire 0.7` for interactive prompts, `toml 0.8` for config, `dirs 6` for config-path resolution.

**Skills (MCP servers)**
- Skills are external MCP servers reached over HTTP. `skills/skill-github`, `skills/skill-gmail`, and `skills/skill-google-calendar` are `SKILL.md`-only: the server loads each `SKILL.md` as system-prompt context when the matching skill is registered (see `crates/edgeclaw-server/src/startup.rs::load_skill_context`) and connects to a user-provided URL.
- Primary first-party skill deployment is the Google Workspace MCP container (`ghcr.io/taylorwilsdon/google_workspace_mcp`) declared in `docker-compose.yml`.
- GitHub tools are reached via `SKILL_GITHUB_URL` (e.g., GitHub's Copilot MCP endpoint) with `SKILL_GITHUB_AUTH_TOKEN`.

**Persistence**
- Single SQLite file (default `edgeclaw.db`, `DATABASE_URL` overridable).
- Migrations `0001_initial.sql` through `0005_souls.sql` under `crates/edgeclaw-server/migrations/`.

**Deployment**
- `docker/Dockerfile.server` (cargo-chef, Alpine, non-root) builds the server image.
- `docker-compose.yml` runs `agent`, `workspace-mcp` (Google Workspace MCP image), and `caddy` (2-alpine) as a reverse proxy.
- `docker-compose.prod.yml` overrides the `agent` service to pull `ghcr.io/adrianmoses/edgeclaw:latest`.
- `deploy/Caddyfile` reverse-proxies `{$DOMAIN}:80` to `agent:8080`.
- `deploy/edgeclaw.service` is a oneshot systemd unit that wraps `docker compose up -d` / `down`.

## Testing Suite

- **Unit tests** (`cargo test --workspace`) live inside `#[cfg(test)]` modules in each crate:
  - `agent-core`: `types.rs`, `soul.rs`, `permissions.rs` — soul parse/compose, permission-chain behavior, type helpers.
  - `edgeclaw-cli`: `config/mod.rs` — config path resolution, env-var overrides, atomic save round-trip.
  - `credential-store`, `skill-registry`, `mcp-client`: per-crate unit tests.
- **Integration tests**: `crates/edgeclaw-server/tests/integration.rs` (~791 lines). In-process mock Anthropic server and mock MCP skill server, driving the real axum router. Fixtures under `tests/fixtures/` (e.g., `end_turn_response.json`, `tool_use_response.json`). Covers health, `/message` and history, skill add/list/remove, approval flow, multi-turn, clear history, scheduled tasks (one-shot, cron, invalid cron), list/delete tasks.
- **Mock pattern**: `MockHttpBackend` with `RefCell<VecDeque<Vec<u8>>>` for pre-recorded Anthropic responses (documented in `CLAUDE.md`).
- **Lint gates**: `cargo clippy -p agent-core -- -D warnings`, same for `edgeclaw-server`; `cargo fmt --all -- --check`.
- **Empty placeholder**: top-level `tests/integration/` directory exists but contains no runnable tests — the real integration suite is the per-crate `edgeclaw-server/tests/integration.rs`. Worth cleaning up.

## Audit Notes

### Capabilities Observed

- Anthropic Messages-API ReAct loop with `max_iterations` (default 10) and `max_continuations` on `MaxTokens` stop reason (default 3).
- Six built-in tools: `bash`, `file_read`, `file_write`, `file_edit`, `glob`, `grep`.
- Four-layer `PolicyChain` permission system: deny-list (destructive shell patterns) → allow-list (read-only tools auto-approved) → destructive-pattern heuristic (names containing `delete`/`remove`/`send`/`drop`) → default-requires-approval catch-all.
- MCP skill routing via `skill-registry` using `skill:tool` namespaced names.
- WebSocket session layer with typed `ServerMessage` / `ClientMessage` enums and a `pending_approvals` map keyed by `request_id` using `oneshot` channels to unblock the agent loop. 5-minute auto-deny timeout on unanswered approvals.
- `POST /message` HTTP fallback that forces `AutoApprove` (no approval prompts).
- Background scheduler that polls `scheduled_tasks` every 10 seconds; supports one-shot (`run_at`) and cron expressions; spawns turns via `run_agent_turn`.
- OAuth 2.0 + PKCE for skill-level credentials; providers: GitHub and Google loaded from env. Encrypted storage (AES-256-GCM, HKDF) in `credentials` table with per-user salt.
- Soul management: DB-backed (`souls` table via migration 0005), YAML-frontmatter `SOUL.md` format, REST (`GET/POST/PATCH /soul`, `POST /soul/generate`), and CLI (`soul show|set|edit|generate|import|export`).
- CLI entry points: `edgeclaw serve`, `edgeclaw chat` (inline raw-mode TUI, auto-spawns server), `edgeclaw config`, `edgeclaw soul`.

### Gaps and Inconsistencies

- **`docs/archive/` still contains Cloudflare-era specs** (`EDGECLAW_SPEC_CF.md`, `EDGECLAW_CREDENTIALS_SPEC_CF.md`, `EDGECLAW_TUI_SPEC_CF.md`) alongside VPS-migration docs. These are intentional history; flagged here only for reader orientation.
- **`skill-github`, `skill-gmail`, `skill-google-calendar` are spec-only.** Only `SKILL.md` files exist; there is no first-party implementation. Gmail and Calendar are reached today via the external `ghcr.io/taylorwilsdon/google_workspace_mcp` container; GitHub is reached via `SKILL_GITHUB_URL`. Confirm whether first-party implementations are planned or whether delegation to third-party MCP servers is permanent.
- **Hardcoded default model.** Default Anthropic model is `claude-sonnet-4-20250514` (fallback when env var missing). `CLAUDE.md` and `README.md` both reference making the model "configurable via `DEFAULT_MODEL` / `CLAUDE_MODEL`" — check for stale naming across code and docs.
- **Phase-1 CLI items not yet implemented** (features 020–022 in `docs/specs/ROADMAP.md`): `--daemonize`, `serve status`/`stop`/`restart`, TLS flags, pipe mode (`--no-tui`), `--session-id`, `--agent`, multiline input (`Ctrl-J`), `[a]` approve-all.
- **Connector management (Telegram/Discord/Slack)** listed in the CLI config spec but not present in the server handlers — config-side skeleton without a server-side listener would be a dead UX.
- **`tests/integration/` at the repo root is empty.** Either delete it or move the `edgeclaw-server/tests/integration.rs` suite there and rename it so the convention matches expectations.
- **All crate versions are `0.1.0`.** No semver discipline yet; fine for pre-release but worth flagging before publishing.
- **Stray `edgeclaw-dev.db` in the repo root.** A development SQLite file is present; `.gitignore` does cover `*.db`, so it is not committed — but consider whether it should live in `/data/` or a scratch dir.

### Uncertain Areas

- Whether EdgeClaw is a personal project or intended to be distributed (the `ghcr.io/adrianmoses/edgeclaw:latest` prod image suggests at least author-use-across-machines distribution; the lack of auth suggests single-operator).
- Whether OAuth refresh-token rotation is implemented or just token storage is — `oauth.rs` has PKCE primitives and `credentials` has `expires_at`, but refresh behavior wasn't directly verified.
- Whether `mcp-client` (workspace member) is actively used or superseded by `skill-registry`'s own routing.
- Whether `MAX_TASKS_PER_USER` (documented) is enforced (not confirmed in scheduler code).
