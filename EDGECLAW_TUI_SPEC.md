# EdgeClaw — Onboarding TUI Specification

> A native Rust TUI for first-run setup and ongoing agent management, built with `inquire` for the wizard flow and `ratatui` for the persistent management dashboard.

---

## Overview

OpenClaw's onboarding is a web UI that requires a running server before you can configure it. EdgeClaw's onboarding is a single native binary — `edgeclaw-cli` — that a user downloads, runs, and completes setup in under two minutes. It handles everything: Cloudflare account connection, agent naming, model selection, API key configuration, initial skill installation, and deployment.

The `edgeclaw-cli` binary serves two purposes:

1. **Setup wizard** (`edgeclaw setup`) — a sequential `inquire`-driven flow run once on first install. Walks the user through all configuration, writes a local `edgeclaw.toml`, and deploys to Cloudflare.
2. **Management TUI** (`edgeclaw manage`) — a persistent `ratatui` dashboard for ongoing operations: monitoring agent status, managing skills, rotating keys, and tailing logs.

---

## Why Two Libraries

`inquire` and `ratatui` serve different interaction models and should not be conflated:

| | `inquire` | `ratatui` |
|---|---|---|
| Model | Sequential, blocking prompts | Immediate-mode render loop |
| Best for | Wizards, one-shot configuration | Dashboards, real-time state |
| Terminal control | Takes over stdin/stdout per prompt | Owns the full alternate screen |
| Async | Not needed — blocking is correct for setup | Pairs with `tokio` for event loop |
| Composability | Linear — each prompt yields a value | Component-based layout |

The wizard is inherently sequential: each step depends on the previous. `inquire` is exactly right for this. The management TUI is stateful and reactive: you are watching live data and issuing commands. `ratatui` is exactly right for this.

Combining them in the same binary is fine — they do not conflict. The wizard runs first in the normal terminal, then exits. The management TUI launches separately in the alternate screen.

---

## Repository Location

```
edgeclaw/
├── crates/
│   └── edgeclaw-cli/        # This spec
│       ├── src/
│       │   ├── main.rs
│       │   ├── wizard/      # inquire-based setup flow
│       │   │   ├── mod.rs
│       │   │   ├── account.rs
│       │   │   ├── agent.rs
│       │   │   ├── model.rs
│       │   │   └── skills.rs
│       │   ├── tui/         # ratatui management dashboard
│       │   │   ├── mod.rs
│       │   │   ├── app.rs
│       │   │   ├── screens/
│       │   │   └── widgets/
│       │   └── config.rs    # edgeclaw.toml read/write
│       └── Cargo.toml
```

---

## Part 1 — Setup Wizard (`edgeclaw setup`)

### 1.1 — Entry Point and Flow Control

The wizard is invoked with `edgeclaw setup`. It detects whether an `edgeclaw.toml` already exists in the current directory or `~/.config/edgeclaw/`. If one is found, it asks whether to reconfigure or exit. This prevents accidental re-runs from clobbering a working deployment.

The wizard is divided into four stages, each in its own module. Each stage returns a typed config struct. The stages run strictly in order:

```
Stage 1: Cloudflare Account
Stage 2: Agent Identity
Stage 3: LLM Model Selection
Stage 4: Skill Installation
─────────────────────────────
       Deploy to Cloudflare
```

If any stage fails (e.g. invalid API key, network error), the wizard prints a clear error, explains what to fix, and re-prompts that stage rather than aborting. Partial progress is not written to disk until all four stages complete successfully — or until the user explicitly saves a draft with `Ctrl+S`.

---

### 1.2 — Stage 1: Cloudflare Account

This stage collects everything needed to deploy to a Cloudflare account and verifies each value before proceeding.

**Prompts (in order):**

```
? Cloudflare Account ID  ›  _______________
  (Find this at: dash.cloudflare.com → right sidebar under "Account ID")

? Cloudflare API Token  ›  ••••••••••••••••••
  (Needs: Workers Scripts:Edit, Durable Objects:Edit)

  ✓ Verifying API token...  [live check via Cloudflare API]

? Workers subdomain  ›  [auto-detected from account, shown as default]
  Your agent will be reachable at: edgeclaw.{subdomain}.workers.dev

? Deploy region preference
  › Global (recommended) — Cloudflare picks closest location
    EU only — useful if GDPR data residency matters
    US only
```

The API token is collected with `inquire::Password` (masked input). After collection, the wizard immediately calls the Cloudflare `/user/tokens/verify` endpoint. If the token lacks required permissions, it shows exactly which permissions are missing rather than a generic error.

The Workers subdomain is fetched from the account after the token is verified, so the user sees their actual subdomain as the default and only needs to confirm it.

---

### 1.3 — Stage 2: Agent Identity

```
? What should your agent be called?  ›  _______________
  (This is how your agent will introduce itself. Can be changed later.)
  Examples: Aria, Max, Friday, Jarvis

? Write a short description of your agent's role (optional)  ›  _______________
  Examples:
    "My personal assistant for email, calendar, and code"
    "Engineering team assistant with GitHub and Linear access"
  Press Enter to skip and use the default.

? What messaging interface will you use?
  › Telegram (recommended)
    Direct HTTP (no messaging integration)
    Skip for now

[If Telegram selected:]
? Telegram Bot Token  ›  ••••••••••••••••••
  (Create a bot at t.me/BotFather and paste the token here)

  ✓ Verifying bot token...  [live check via Telegram API]
  ✓ Bot name: @YourBotName

? Your Telegram user ID  ›  _______________
  (Send /getid to @userinfobot in Telegram to find this)
  This is the only Telegram account that can command your agent.
```

The agent name is used as the `system_prompt` preamble and stored in the deployed Worker's `vars`. The Telegram bot token and user ID are stored as Worker secrets, never in `edgeclaw.toml` on disk (only a reference `telegram_configured = true` is stored locally).

---

### 1.4 — Stage 3: LLM Model Selection

```
? Choose your default LLM model

  ┌─────────────────────────────────────────────────────────────────┐
  │  Model                    Speed      Cost      Context          │
  │  ─────────────────────────────────────────────────────────────  │
  │  claude-opus-4-6          Slower     Higher    200k tokens      │
  │  claude-sonnet-4-6 ★      Balanced   Mid       200k tokens      │
  │  claude-haiku-4-5         Fastest    Lower     200k tokens      │
  └─────────────────────────────────────────────────────────────────┘

  › claude-sonnet-4-6 (recommended)
    claude-opus-4-6
    claude-haiku-4-5

? Anthropic API Key  ›  ••••••••••••••••••
  (Create one at: console.anthropic.com/settings/keys)

  ✓ Verifying API key...  [test completion call with 1 token]
  ✓ Key valid. Model access confirmed.

? Maximum tokens per response  ›  1024
  (Controls response length and cost per turn. 1024 is a good default.)

? Maximum tool-call iterations per turn  ›  10
  (Prevents runaway agent loops. Recommended: 5–15)
```

The model list is hardcoded from the spec but structured so new models can be added by editing a single `MODELS` constant. The verification call uses a minimal prompt ("hi") with `max_tokens: 1` — enough to confirm key validity and model access without spending meaningfully on the user's quota.

The `max_iterations` field maps directly to `AgentConfig::max_iterations` in `agent-core`.

---

### 1.5 — Stage 4: Skill Installation

This stage uses a multi-select prompt to let the user choose which skills to install now. Skills can be added later via `edgeclaw manage` or by telling the agent in chat.

```
? Select skills to install now  (Space to select, Enter to confirm)

  ──── Productivity ────
  [ ] Gmail           Read and send email on your behalf
  [ ] Google Calendar  Check and create calendar events
  [ ] Notion          Read and write Notion pages and databases

  ──── Development ────
  [ ] GitHub          Read repos, issues, and PRs; create issues
  [ ] Linear          Manage issues and projects

  ──── Utilities ────
  [x] Web Search      Search the web (uses Brave Search API)    ← default on
  [x] HTTP Fetch      Fetch and read web pages                  ← default on
  [ ] Memory          Persistent cross-session memory (SQLite)

  ──── Custom ────
  [ ] Add custom MCP server...

  (You can add more skills at any time after setup)
```

`Web Search` and `HTTP Fetch` are pre-selected because they require no OAuth and are immediately useful. All OAuth-requiring skills are unselected by default.

For each OAuth-requiring skill selected, the wizard collects the necessary credential:

```
[If Gmail or Google Calendar selected:]

  ─── Google OAuth Setup ───
  You selected: Gmail, Google Calendar

  These skills share a single Google OAuth connection.

  ? Google OAuth Client ID  ›  _______________
    (Create at: console.cloud.google.com/apis/credentials)
    Required scopes: Gmail API, Google Calendar API

  ? Google OAuth Client Secret  ›  ••••••••••••••••••

  Note: You will authorise your Google account after deployment,
  by telling your agent: "connect my Google account"

[If GitHub selected:]

  ─── GitHub OAuth Setup ───
  ? GitHub OAuth App Client ID  ›  _______________
    (Create at: github.com/settings/developers → OAuth Apps → New)
    Callback URL: https://edgeclaw.{subdomain}.workers.dev/oauth/callback

  ? GitHub OAuth App Client Secret  ›  ••••••••••••••••••

[If custom MCP server selected:]

  ─── Custom MCP Server ───
  ? Skill name  ›  _______________
  ? MCP server URL  ›  https://_______________
  ? Authentication (if required)
    › None
      Bearer token
      Basic auth

  ✓ Connecting to MCP server...
  ✓ Discovered 4 tools: [tool_a, tool_b, tool_c, tool_d]
```

Skill credentials (OAuth client IDs and secrets) are stored as Worker secrets, not in `edgeclaw.toml`. The local config only stores which skills were selected, not their credentials.

If `Web Search` is selected, the wizard immediately asks for the Brave Search API key (or Tavily, with a selection prompt). This is the only skill that requires a key at setup time rather than later OAuth flow.

---

### 1.6 — Pre-Deployment Summary and Confirmation

Before deploying, the wizard renders a full summary of everything that will be configured:

```
┌─────────────────────────────────────────────────────────────────────┐
│  EdgeClaw Setup Summary                                             │
│─────────────────────────────────────────────────────────────────────│
│  Account      acme-corp.workers.dev                                 │
│  Agent name   Aria                                                  │
│  Model        claude-sonnet-4-6                                     │
│  Interface    Telegram (@AriaBotName)                               │
│  Skills       Web Search, HTTP Fetch, Gmail, GitHub                 │
│─────────────────────────────────────────────────────────────────────│
│  Will deploy:                                                       │
│    Workers:   edgeclaw (dispatcher + AgentDO)                       │
│               skill-web-search                                      │
│               skill-http-fetch                                      │
│               skill-gmail                                           │
│               skill-github                                          │
│               skill-oauth-callback                                  │
│    Secrets:   ANTHROPIC_API_KEY, TOKEN_MASTER_KEY,                  │
│               TELEGRAM_BOT_TOKEN, GITHUB_CLIENT_SECRET,             │
│               GOOGLE_CLIENT_SECRET, BRAVE_API_KEY                   │
│─────────────────────────────────────────────────────────────────────│
│  Estimated monthly cost (Cloudflare Workers Paid):                  │
│    $5/mo base + usage (typically <$1/mo for personal use)           │
└─────────────────────────────────────────────────────────────────────┘

? Deploy now?  › Yes / Edit / Cancel
```

The cost estimate is a static string based on known Cloudflare pricing, not a live calculation. "Edit" returns to the beginning of the relevant stage. "Cancel" exits without writing anything.

---

### 1.7 — Deployment

If the user confirms, the wizard runs the deployment sequence with a live progress display:

```
  Deploying EdgeClaw...

  [✓] Generating TOKEN_MASTER_KEY
  [✓] Writing wrangler.toml
  [✓] Building edgeclaw-worker (Rust → WASM)           ~20s
  [✓] Building skill-web-search                         ~5s
  [✓] Building skill-http-fetch                         ~5s
  [✓] Building skill-gmail                              ~5s
  [✓] Building skill-github                             ~5s
  [✓] Deploying Workers to Cloudflare                   ~10s
  [✓] Setting secrets (8 values)
  [✓] Running Durable Object migrations
  [✓] Verifying deployment (health check)

  ────────────────────────────────────────────────────────
  ✓ EdgeClaw is live!

  Your agent is at:  https://edgeclaw.acme-corp.workers.dev
  Telegram:          @AriaBotName — send it a message now!

  Next steps:
    • Send "hello" to your bot to verify it's working
    • Connect Google: tell your agent "connect my Google account"
    • Run `edgeclaw manage` to open the management dashboard

  Config saved to: ~/.config/edgeclaw/edgeclaw.toml
```

The deployment step shells out to `wrangler` (which must be installed, or the wizard installs it via `npm` if Node is present). The progress indicators are driven by parsing `wrangler` stdout — each `[✓]` is printed as the corresponding step completes, not all at once.

`TOKEN_MASTER_KEY` is generated by the wizard itself using `rand::random::<[u8; 32]>()`, base64-encoded, and passed to `wrangler secret put` via stdin. It is never written to disk.

---

### 1.8 — Wizard Crate Dependencies

```toml
# crates/edgeclaw-cli/Cargo.toml
[dependencies]
inquire     = "0.7"
ratatui     = "0.29"
crossterm   = "0.28"
clap        = { version = "4", features = ["derive"] }
tokio       = { version = "1", features = ["full"] }
serde       = { version = "1", features = ["derive"] }
toml        = "0.8"
reqwest     = { version = "0.12", features = ["json"] }
rand        = { version = "0.8", features = ["getrandom"] }
base64      = "0.22"
anyhow      = "1"
indicatif   = "0.17"   # progress bars for the deployment step
```

---

## Part 2 — Management TUI (`edgeclaw manage`)

### 2.1 — Overview

After setup, `edgeclaw manage` opens a persistent `ratatui` dashboard in the alternate screen. This is the day-to-day interface for monitoring and managing the deployed agent — not for chatting with it (that happens in Telegram or the HTTP interface).

Layout:

```
┌──────────────────────────────────────────────────────────────────────┐
│  EdgeClaw  [agent: Aria]  [model: claude-sonnet-4-6]  [● live]       │
├───────────────┬──────────────────────────────────────────────────────┤
│  Navigation   │                                                       │
│               │                                                       │
│  > Status     │            [Main Content Area]                        │
│    Skills     │                                                       │
│    Logs       │                                                       │
│    Settings   │                                                       │
│    Secrets    │                                                       │
│               │                                                       │
├───────────────┴──────────────────────────────────────────────────────┤
│  [Tab] Navigate  [Enter] Select  [q] Quit  [?] Help                  │
└──────────────────────────────────────────────────────────────────────┘
```

Navigation is via `Tab`/`Shift+Tab` or `j`/`k` (vim-style). `Enter` selects. `Esc` goes back. `q` quits.

---

### 2.2 — Screen: Status

The default screen. Pulls live data from the deployed agent via the Cloudflare API.

```
  Agent Status
  ────────────────────────────────────────────────────────────
  Name          Aria
  URL           https://edgeclaw.acme-corp.workers.dev
  Model         claude-sonnet-4-6  (change)
  Telegram      @AriaBotName  ✓ connected
  Deployment    2026-03-12 14:22 UTC  (3 days ago)
  Worker ver.   a3f9c1b

  Durable Objects
  ────────────────────────────────────────────────────────────
  AgentDO instances      2 active
  OAuthDO instances      0 active
  MemorySkillDO          2 active
  Total SQLite storage   1.2 MB / 10 GB

  Last 24h Activity
  ────────────────────────────────────────────────────────────
  Messages processed     47
  Tool calls made        123
  Skills invoked         gmail (31), github (28), web-search (64)
  Errors                 0
```

Activity numbers are fetched from Cloudflare Workers Analytics API. Durable Object counts are fetched via a dedicated `/admin/status` endpoint on the dispatcher Worker, protected by the Cloudflare API token.

---

### 2.3 — Screen: Skills

```
  Installed Skills
  ────────────────────────────────────────────────────────────
  Name              Status    Auth         Last used
  ────────────────────────────────────────────────────────────
  web-search        ✓ live    API key      2 min ago
  http-fetch        ✓ live    None         14 min ago
  gmail             ✓ live    OAuth ✓      1 hour ago
  github            ✓ live    OAuth ✓      3 hours ago
  google-calendar   ✗ not installed

  [a] Add skill   [r] Remove selected   [Enter] View details
  ────────────────────────────────────────────────────────────
```

Selecting a skill with `Enter` shows a detail pane:

```
  ── gmail ───────────────────────────────────────────────────
  Status           Live
  MCP URL          https://skill-gmail.acme-corp.workers.dev
  OAuth user       user@example.com
  Scopes           gmail.readonly, gmail.send, gmail.modify
  Token expires    in 42 minutes (auto-refresh enabled)
  Tools (8)        gmail_list_messages, gmail_get_message,
                   gmail_search, gmail_send (+5 more)

  [d] Disconnect OAuth   [u] Uninstall   [Esc] Back
```

Pressing `a` to add a skill opens an `inquire` prompt inline (the ratatui frame drops to normal terminal mode temporarily, `inquire` runs, then ratatui resumes). This is the same multi-select from the setup wizard but running post-deployment.

---

### 2.4 — Screen: Logs

```
  Worker Logs  [Live]  ─────────────────────────────────────────────
  Filter: ___________  [Enter to apply]   [p] Pause   [c] Clear

  2026-03-15 14:33:01  INFO  [AgentDO:agent:123]  Message received
  2026-03-15 14:33:01  INFO  [AgentDO:agent:123]  LLM call started (claude-sonnet-4-6)
  2026-03-15 14:33:03  INFO  [AgentDO:agent:123]  Tool call: gmail_search (query="unread from boss")
  2026-03-15 14:33:04  INFO  [AgentDO:agent:123]  Tool result: 3 messages found
  2026-03-15 14:33:04  INFO  [AgentDO:agent:123]  LLM resumed
  2026-03-15 14:33:06  INFO  [AgentDO:agent:123]  Response sent (2341ms total)
  2026-03-15 14:33:06  INFO  [AgentDO:agent:123]  Credential refresh: gmail (ok)
  ...

  ↓ auto-scroll  [f] Filter  [/] Search  [q] Back
```

Logs are streamed from Cloudflare Workers Logs API using `reqwest` in a `tokio` background task, pushing log lines into a `mpsc` channel that the ratatui render loop consumes. The filter field is a live substring match applied client-side.

---

### 2.5 — Screen: Settings

```
  Settings
  ────────────────────────────────────────────────────────────
  Agent name          Aria                     [edit]
  System prompt       "You are Aria, a per..."  [edit]
  Default model       claude-sonnet-4-6         [change]
  Max iterations      10                        [edit]
  Max tokens          1024                      [edit]
  Telegram user ID    123456789                 [edit]

  Deployment
  ────────────────────────────────────────────────────────────
  Cloudflare account  acme-corp
  Workers subdomain   acme-corp.workers.dev
  Region              Global

  [Enter] Edit field   [s] Save and redeploy   [Esc] Back
```

Editing a setting opens an `inquire` text prompt. "Save and redeploy" runs `wrangler deploy` in the background with a progress overlay. Settings that require redeployment (anything stored as Worker `vars`) are marked with a `*` after editing but before saving.

---

### 2.6 — Screen: Secrets

```
  Secrets
  ────────────────────────────────────────────────────────────
  ANTHROPIC_API_KEY    ••••••••••••  set 2026-03-12   [rotate]
  TOKEN_MASTER_KEY     ••••••••••••  set 2026-03-12   [rotate]
  TELEGRAM_BOT_TOKEN   ••••••••••••  set 2026-03-12   [rotate]
  GITHUB_CLIENT_SECRET ••••••••••••  set 2026-03-12   [rotate]
  GOOGLE_CLIENT_SECRET ••••••••••••  set 2026-03-12   [rotate]
  BRAVE_API_KEY        ••••••••••••  set 2026-03-12   [rotate]

  ⚠ Rotating TOKEN_MASTER_KEY will invalidate all stored OAuth tokens.
    Users will need to re-authorise each connected skill.

  [r] Rotate selected   [Esc] Back
```

Secret values are never displayed — only masked indicators and set-dates are shown (fetched from the Cloudflare API which returns metadata but not values). "Rotate" opens an `inquire::Password` prompt, then calls `wrangler secret put` under the hood.

The `TOKEN_MASTER_KEY` rotation warning is shown whenever that row is selected, because its implications (all user OAuth tokens become invalid) are non-obvious.

---

## Part 3 — Local Config File

`edgeclaw.toml` stores non-secret configuration. It is written after a successful setup and read by `edgeclaw manage`. Secrets are never written here.

```toml
[agent]
name = "Aria"
system_prompt = "You are Aria, a personal assistant."
model = "claude-sonnet-4-6"
max_tokens = 1024
max_iterations = 10

[cloudflare]
account_id = "abc123..."
subdomain = "acme-corp"
region = "global"

[telegram]
configured = true
# bot_token stored as Worker secret, not here

[skills]
installed = ["web-search", "http-fetch", "gmail", "github"]

[skills.web-search]
provider = "brave"
# api_key stored as Worker secret

[skills.gmail]
oauth_configured = false   # true after user completes the OAuth flow in chat

[skills.github]
oauth_configured = false
```

The config file is stored at `~/.config/edgeclaw/edgeclaw.toml` by default, or in the current directory if the user ran `edgeclaw setup` from a project directory. `edgeclaw manage` checks both locations.

---

## Milestones

| Milestone | Description | Done When |
|---|---|---|
| M4.1 | `clap` entry point: `setup`, `manage`, `--help` all parse correctly | `cargo test` for CLI parsing passes |
| M4.2 | Stage 1: Cloudflare token prompt, live verify call, subdomain fetch | Manual run with real CF account succeeds |
| M4.3 | Stage 2: Agent name and Telegram bot token collected and verified | Bot token verified via Telegram API |
| M4.4 | Stage 3: Model selection renders table, API key verified with 1-token call | Live verify against Anthropic API passes |
| M4.5 | Stage 4: Multi-select skill picker, OAuth credential collection per skill | All three OAuth flows collect correctly |
| M4.6 | Pre-deployment summary renders all collected config | Visual review confirms all fields present |
| M4.7 | Deployment shells out to `wrangler`, streams progress, confirms live | Full end-to-end deploy from `edgeclaw setup` |
| M4.8 | `edgeclaw.toml` written correctly, no secrets in file | Config file audit: no secret values present |
| M4.9 | `ratatui` management TUI launches with Status screen live data | Status screen renders with real CF API data |
| M4.10 | Skills screen shows installed skills with OAuth status | skill-gmail and skill-github shown correctly |
| M4.11 | Logs screen streams live Worker logs with filter | Logs visible within 5s of a Telegram message |
| M4.12 | Settings screen edits and redeploys a changed field | Model change reflected in deployed Worker |
| M4.13 | Secrets screen rotates ANTHROPIC_API_KEY without touching others | Only target secret updated via wrangler |
| M4.14 | `TOKEN_MASTER_KEY` rotation warning shown before rotating | Warning text visible when row selected |
