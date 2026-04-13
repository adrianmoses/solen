# EdgeClaw CLI Specification

> Version: 0.1.0-draft  
> Last updated: 2026-04-13

---

## Overview

EdgeClaw is a self-hosted agent runtime with a WebSocket-based communication layer that supports bidirectional tool approval within a session. The CLI is structured around three top-level verbs with distinct operational lifetimes:

| Verb | Lifetime | Purpose |
|------|----------|---------|
| `serve` | long-running daemon | Start and manage the WebSocket server |
| `chat` | ephemeral session | Attach a TUI or pipe-mode client to a server |
| `config` | instant / interactive | Read and write agent configuration |

All persistent state lives in a TOML config file. The default path is `~/.config/edgeclaw/config.toml`, overridable via `--config <path>` or the `EDGECLAW_CONFIG` environment variable.

---

## Global flags

These flags are accepted by every command and subcommand.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--config <path>` | path | `~/.config/edgeclaw/config.toml` | Path to config file |
| `--log-level <level>` | enum | `info` | One of `error`, `warn`, `info`, `debug`, `trace` |
| `--json` | bool | false | Emit structured JSON logs instead of human-readable output |
| `--help` | bool | — | Print help for the current command |
| `--version` | bool | — | Print version and exit |

---

## `edgeclaw serve`

Start the EdgeClaw WebSocket server. All chat clients (TUI, Telegram, Discord, Slack) connect to this server. Tool approval events are brokered over the WebSocket connection, requiring it to be bidirectional.

```
edgeclaw serve [OPTIONS]
```

### Options

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--port <port>` | u16 | `7100` | Port to listen on |
| `--host <host>` | string | `127.0.0.1` | Bind address |
| `--daemonize` | bool | false | Fork into background after startup |
| `--tls-cert <path>` | path | — | Path to TLS certificate (PEM). Requires `--tls-key` |
| `--tls-key <path>` | path | — | Path to TLS private key (PEM). Requires `--tls-cert` |
| `--pid-file <path>` | path | — | Write PID to file on startup (useful with `--daemonize`) |

### Lifecycle subcommands

```
edgeclaw serve status
edgeclaw serve stop
edgeclaw serve restart
```

| Subcommand | Description |
|------------|-------------|
| `status` | Print whether the server is running, its PID, and bound address |
| `stop` | Gracefully shut down a daemonized server |
| `restart` | Stop then start the daemonized server, reloading config |

### Example

```sh
# Foreground, default port
edgeclaw serve

# Background on a custom port with TLS
edgeclaw serve --port 7443 --tls-cert ./certs/cert.pem --tls-key ./certs/key.pem --daemonize
```

---

## `edgeclaw chat`

Open a local chat session with an EdgeClaw agent. This command is the zero-config entry point: if no server is found on the default address, it automatically spawns one in-process, connects to it, and tears it down on exit.

```
edgeclaw chat [OPTIONS]
```

### Spawn-or-attach behavior

1. Attempt a WebSocket connect to `--connect` (default: `ws://127.0.0.1:7100`).
2. If the connection succeeds, attach to the running server. The server is left running on exit.
3. If the connection is refused, spawn an in-process server on the default port, connect to it, and shut it down when the TUI exits.

### Options

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--connect <url>` | URL | `ws://127.0.0.1:7100` | WebSocket URL of a running server |
| `--agent <name>` | string | `default` | Agent personality to use (must exist in config) |
| `--no-tui` | bool | false | Disable the TUI; use stdin/stdout line mode for piping or scripting |
| `--session-id <id>` | string | — | Resume a named session if the server supports session persistence |

### TUI key bindings

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Ctrl-J` | Insert newline |
| `Ctrl-C` | Cancel in-flight request |
| `Ctrl-D` | Exit session |
| `Ctrl-L` | Clear screen |
| `Tab` | Cycle focus between input and history pane |
| `y` / `n` | Approve / deny a pending tool call (when approval prompt is active) |

### Tool approval UX

When the server emits a tool approval request, the TUI suspends normal input and renders an approval prompt inline:

```
┌─ Tool approval ─────────────────────────────────────────────┐
│ bash_tool                                                     │
│ $ rm -rf ./build                                              │
│                                                               │
│  [y] Approve   [n] Deny   [a] Approve all in session         │
└───────────────────────────────────────────────────────────────┘
```

### Example

```sh
# Zero-config: spawns server automatically if none is running
edgeclaw chat

# Attach to a remote server
edgeclaw chat --connect ws://my-server.example.com:7100 --agent friday

# Pipe mode: send a one-shot prompt and capture output
echo "Summarise today's standup" | edgeclaw chat --no-tui
```

---

## `edgeclaw config`

Read and write agent configuration. When invoked with no subcommand, launches an interactive setup flow:

- **First run** (no config file found): guided wizard covering model, personality, and optional connectors.
- **Subsequent runs**: presents a menu of domains with their current values, allowing targeted edits.

Named subcommands always bypass the interactive flow and are safe to use in scripts.

```
edgeclaw config [SUBCOMMAND]
```

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `show` | Pretty-print the full config |
| `edit` | Open the config file in `$EDITOR` |
| `set <domain> [OPTIONS]` | Write values in a domain |
| `connector <subcommand>` | Manage messaging connectors |

---

### `edgeclaw config show`

Print the resolved config to stdout. Secrets (tokens, API keys) are redacted by default.

```
edgeclaw config show [--reveal-secrets]
```

| Flag | Description |
|------|-------------|
| `--reveal-secrets` | Print secrets in plaintext (use with care) |

---

### `edgeclaw config edit`

Open the raw TOML config file in `$EDITOR` (falling back to `vi`). Validates the file after the editor closes and prints any parse errors.

```
edgeclaw config edit
```

---

### `edgeclaw config set`

Write values in a specific domain. Each domain maps to a TOML table in the config file.

#### `config set model`

```
edgeclaw config set model [OPTIONS]
```

| Flag | Type | Description |
|------|------|-------------|
| `--provider <name>` | string | Model provider (e.g. `anthropic`, `openai`, `ollama`) |
| `--model <id>` | string | Model identifier (e.g. `claude-sonnet-4-20250514`) |
| `--api-key <key>` | string | API key for the provider (stored in config, consider env var) |
| `--base-url <url>` | URL | Override the provider base URL (useful for local/proxy endpoints) |
| `--max-tokens <n>` | u32 | Max tokens per response |
| `--temperature <f>` | f32 | Sampling temperature (0.0–2.0) |

**Corresponding TOML:**
```toml
[model]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "sk-ant-..."
max_tokens = 8096
temperature = 1.0
```

#### `config set personality`

```
edgeclaw config set personality [OPTIONS]
```

| Flag | Type | Description |
|------|------|-------------|
| `--name <name>` | string | Identifier for this personality |
| `--system-prompt <text>` | string | System prompt text |
| `--system-prompt-file <path>` | path | Load system prompt from a file |

Multiple personalities can be defined in the config file as an array of tables and selected at runtime with `chat --agent <name>`.

**Corresponding TOML:**
```toml
[[personalities]]
name = "default"
system_prompt = "You are a helpful assistant."

[[personalities]]
name = "friday"
system_prompt = "You are Friday, a concise and direct engineering assistant."
```

#### `config set approval`

```
edgeclaw config set approval --mode <mode>
```

| Mode | Description |
|------|-------------|
| `always-ask` | Prompt the user for every tool call (default) |
| `auto-approve` | Approve all tool calls without prompting |
| `deny-all` | Deny all tool calls without prompting |
| `allowlist` | Approve tools listed under `[approval.allowed_tools]`, deny everything else |

**Corresponding TOML:**
```toml
[approval]
mode = "always-ask"
allowed_tools = ["read_file", "web_search"]   # only used when mode = "allowlist"
```

#### `config set tools`

```
edgeclaw config set tools [OPTIONS]
```

| Flag | Type | Description |
|------|------|-------------|
| `--enable <tool>` | string | Add a tool to the enabled set |
| `--disable <tool>` | string | Remove a tool from the enabled set |
| `--list` | bool | Print available tools and their enabled status |

**Corresponding TOML:**
```toml
[tools]
enabled = ["bash_tool", "web_search", "read_file", "write_file"]
```

---

### `edgeclaw config connector`

Manage messaging connectors. Connectors are stored as an array of TOML tables. Each has a `type` and type-specific credentials.

```
edgeclaw config connector <subcommand>
```

| Subcommand | Description |
|------------|-------------|
| `add` | Register a new connector |
| `list` | Print all configured connectors |
| `remove <name>` | Deregister a connector by name |
| `test <name>` | Send a test message to verify credentials |

#### `connector add`

```
edgeclaw config connector add --type <type> --name <name> [OPTIONS]
```

**Telegram:**
```
edgeclaw config connector add --type telegram --name my-bot --token <BOT_TOKEN>
```

| Flag | Description |
|------|-------------|
| `--token` | Telegram bot token from @BotFather |
| `--allowed-chat-ids` | Comma-separated list of chat IDs to accept messages from |

**Discord:**
```
edgeclaw config connector add --type discord --name my-bot --token <BOT_TOKEN> --guild-id <ID>
```

| Flag | Description |
|------|-------------|
| `--token` | Discord bot token |
| `--guild-id` | Server (guild) ID to join |
| `--channel-id` | Optional: restrict to a specific channel |

**Slack:**
```
edgeclaw config connector add --type slack --name my-bot --app-token <TOKEN> --bot-token <TOKEN>
```

| Flag | Description |
|------|-------------|
| `--app-token` | Slack app-level token (for Socket Mode) |
| `--bot-token` | Slack bot OAuth token |
| `--channel` | Optional: restrict to a specific channel name or ID |

**Corresponding TOML:**
```toml
[[connectors]]
name = "my-telegram"
type = "telegram"
token = "123456:ABC-..."
allowed_chat_ids = [987654321]

[[connectors]]
name = "my-discord"
type = "discord"
token = "MTI3..."
guild_id = "1234567890"

[[connectors]]
name = "my-slack"
type = "slack"
app_token = "xapp-..."
bot_token = "xoxb-..."
channel = "engineering"
```

---

## Config file reference

Full annotated example of `~/.config/edgeclaw/config.toml`:

```toml
# ── Model ───────────────────────────────────────────────────────────────────
[model]
provider    = "anthropic"
model       = "claude-sonnet-4-20250514"
api_key     = "sk-ant-..."        # or set ANTHROPIC_API_KEY in env
max_tokens  = 8096
temperature = 1.0

# ── Server ──────────────────────────────────────────────────────────────────
[server]
host = "127.0.0.1"
port = 7100

# ── Approval ────────────────────────────────────────────────────────────────
[approval]
mode          = "always-ask"       # always-ask | auto-approve | deny-all | allowlist
allowed_tools = []                 # used when mode = "allowlist"

# ── Tools ───────────────────────────────────────────────────────────────────
[tools]
enabled = ["bash_tool", "web_search", "read_file", "write_file"]

# ── Personalities ────────────────────────────────────────────────────────────
[[personalities]]
name          = "default"
system_prompt = "You are a helpful assistant."

[[personalities]]
name          = "friday"
system_prompt = "You are Friday, a concise and direct engineering assistant."

# ── Connectors ───────────────────────────────────────────────────────────────
[[connectors]]
name             = "my-telegram"
type             = "telegram"
token            = "123456:ABC-..."
allowed_chat_ids = [987654321]
```

---

## Environment variables

| Variable | Equivalent flag | Description |
|----------|----------------|-------------|
| `EDGECLAW_CONFIG` | `--config` | Path to config file |
| `EDGECLAW_LOG` | `--log-level` | Log level |
| `ANTHROPIC_API_KEY` | `config set model --api-key` | Anthropic API key (preferred over storing in config) |
| `OPENAI_API_KEY` | — | OpenAI API key when using the OpenAI provider |
| `EDITOR` | — | Editor used by `config edit` |

---

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | General error |
| `2` | Bad arguments or invalid config |
| `3` | Server not reachable |
| `4` | Tool approval denied |
| `130` | Interrupted (Ctrl-C) |
