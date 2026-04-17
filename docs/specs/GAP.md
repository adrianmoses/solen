# CLI Gap Analysis

<!-- status: inferred -->
| Field | Value |
|---|---|
| status | inferred |
| created | 2026-04-17 |
| inferred-from | `edgeclaw-cli-spec.md` (deleted 2026-04-17), `crates/edgeclaw-cli/src/{main,chat,config}*.rs`, `crates/edgeclaw-server/src/{agent,handlers,scheduler,startup}.rs` |

This document captures the deltas between the deleted `edgeclaw-cli-spec.md` (v0.1.0-draft, 2026-04-13) and what is actually implemented in `crates/edgeclaw-cli/`. New feature specs should start from this file, not from the archived CLI spec.

---

## Global flags

| Flag | Status | Notes |
|---|---|---|
| `--config <path>` | implemented | `main.rs:23`; also honors `EDGECLAW_CONFIG`. |
| `--log-level <level>` | implemented | `main.rs:19`. |
| `--help` / `--version` | implemented | clap default. |
| `--json` structured logs | missing | |

---

## `edgeclaw serve`

| Feature | Status | Notes |
|---|---|---|
| `--host`, `--port` (default `127.0.0.1:7100`) | implemented | `main.rs:44-50`. |
| Foreground run | implemented | Delegates to `edgeclaw_server::startup::run_server`. |
| `--daemonize`, `--pid-file` | missing | Users can substitute with `systemd` or `nohup` today. |
| `--tls-cert` / `--tls-key` | missing | Production deployments terminate TLS at Caddy (`deploy/Caddyfile`); consider dropping from the spec. |
| `serve status` / `stop` / `restart` | missing | Depends on `--daemonize` + `--pid-file`. |

---

## `edgeclaw chat`

| Feature | Status | Notes |
|---|---|---|
| `--connect <url>` (default `ws://127.0.0.1:7100/ws`) | implemented | `main.rs:56`. |
| Spawn-or-attach on connect-refused | implemented | `chat/connection.rs:36-78`; spawns in-process server, tears down on exit. |
| Inline raw-mode TUI | implemented | `chat/mod.rs`. |
| Approval prompt with `y` / `n` | implemented | `chat/mod.rs:85-98`. |
| Approval-prompt box UI | implemented | `chat/mod.rs:182-201`. |
| Colored `agent>` / `error>` / `[tool]` | implemented | `chat/mod.rs:173,208,205`. |
| `Ctrl-C` / `Ctrl-D` exit | implemented | `chat/mod.rs:79`. |
| `--no-tui` pipe mode | missing | Blocks scripted use (`echo "prompt" \| edgeclaw chat --no-tui`). |
| `--agent <name>` | missing | Blocked on server consuming `personalities` (see below). |
| `--session-id` | missing | Server has session concepts; this is a thin CLI flag. |
| `Ctrl-J` insert newline | missing | Only `Enter` is handled. |
| `Ctrl-L` clear screen | missing | |
| `[a]` approve-all-in-session | missing | Prompt text at `chat/mod.rs:159` doesn't advertise it. |
| `Tab` cycle focus | should drop from spec | Implies a multi-pane TUI the current raw-mode single-line UI doesn't have. |

---

## `edgeclaw config`

CLI side is largely complete; several sections are written to TOML but **never read by the server**.

| Feature | Status | Notes |
|---|---|---|
| First-run wizard + edit menu | implemented | `config/wizard.rs`. |
| `config show [--reveal-secrets]` | implemented | `config/show.rs`. |
| `config edit` (via `$EDITOR`, `vi` fallback) | implemented | `config/edit.rs`. |
| `config set model` | implemented | `config/set.rs`. Writes `[model]` which the server reads. |
| `config set personality` | CLI only — **server ignores** | `personalities` in TOML is not consumed by the server. |
| `config set approval` | CLI only — **server ignores** | Server's own `ApprovalMode` (`agent.rs:18`) is `AutoApprove` for `/message` + scheduled tasks, `Session` for WebSocket. CLI's `always-ask` / `auto-approve` / `deny-all` / `allowlist` don't feed in. |
| `config set tools --enable/--disable/--list` | CLI only — **server ignores** | `tools.enabled` is not consumed. |
| `config connector add/list/remove/test` | CLI only — **server ignores** | No Telegram/Discord/Slack listener exists in `crates/edgeclaw-server/src/` — connectors are stored but never receive messages. |

---

## Environment variables

| Var | Status | Notes |
|---|---|---|
| `EDGECLAW_CONFIG` | implemented | `config/mod.rs`. |
| `ANTHROPIC_API_KEY` | implemented | Propagated via `main.rs:312`. |
| `EDITOR` | implemented | Used by `config edit`. |
| `EDGECLAW_LOG` | missing | Code uses `RUST_LOG` through `EnvFilter::try_from_default_env`. Trivial alias if still wanted. |
| `OPENAI_API_KEY` | missing | No OpenAI provider wired. Spec artefact — see provider gap below. |

---

## Exit codes

Spec defined `0` / `1` / `2` / `3` / `4` / `130`. Implementation returns anyhow errors from every path, so all non-zero exits are `1`. Distinct codes are **not implemented**.

---

## Spec-level gaps (design, not just missing code)

- **Multi-provider claim is aspirational.** Spec mentions `anthropic`, `openai`, `ollama` as provider options; only `anthropic` has code paths (`llm.rs`, `ReqwestBackend`). Either drop the other names from the spec or plan the abstraction.
- **Personality vs. Soul overlap.** CLI spec defines `config set personality` / `--agent <name>`, but the server has evolved a separate `Soul` system (archetype/tone/verbosity/decision-style in `crates/agent-core/src/soul.rs`, persisted in `souls` table) with its own CLI namespace (`edgeclaw soul`). The spec's "personalities array of tables" is redundant with the soul system. Pick one mental model and retire the other.
- **Connector runtime missing.** The single biggest dead-UX item: users can `config connector add` but no message listener exists on the server. Either build it or remove the CLI subtree.
- **TLS flags redundant with Caddy.** Production deploys terminate TLS at `deploy/Caddyfile`; `--tls-cert` / `--tls-key` on `edgeclaw serve` is unlikely to earn its keep.

---

## Recommended next moves

Ordered roughly by "dead UX → user-facing polish → nice-to-have":

1. Decide connectors in or out. If in, build server-side Telegram/Discord/Slack listeners. If out, delete `config connector` from the CLI and the `[[connectors]]` TOML schema.
2. Reconcile Personality vs. Soul. If Soul wins, delete `config set personality` and `--agent <name>` from the CLI spec; if Personality wins, retire `edgeclaw soul` and the `souls` table.
3. Wire `approval.mode` and `tools.enabled` from `config.toml` into the server, or delete the CLI commands.
4. Chat polish: `--no-tui` pipe mode, `Ctrl-J` multiline, `[a]` approve-all-in-session.
5. `--session-id` on chat.
6. Decide whether multi-provider is real scope. If yes, build the `LlmBackend` abstraction; if no, drop `openai`/`ollama` from spec text and `--provider` help strings.
7. Drop from spec: `Tab` cycle-focus, TLS flags.
8. Nice-to-have: `--daemonize` + `serve status/stop/restart`, distinct exit codes, `--json` logs, `EDGECLAW_LOG`.
