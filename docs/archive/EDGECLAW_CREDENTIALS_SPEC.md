# EdgeClaw — Credential Management Specification

_Original Cloudflare Workers version archived at docs/archive/EDGECLAW_CREDENTIALS_SPEC_CF.md._

> Per-user OAuth credential storage, PKCE-based authorisation flows, and skill installation for Gmail, GitHub, and Google Calendar.

---

## Overview

OpenClaw and NanoClaw treat credential management as an afterthought — tokens land in plaintext config files or unencrypted storage, and the trust boundary between skills is the application layer rather than the cryptographic layer. EdgeClaw takes a different approach: tokens are **never stored in plaintext anywhere**, the encryption key **never exists in storage**, and each skill's authorisation is **scoped and auditable**.

This spec covers three sequential implementation phases:

1. **Envelope encryption** — how per-user OAuth tokens are stored encrypted in `edgeclaw-server`'s SQLite (via `sqlx`), with a two-source key derivation scheme and automatic token refresh.
2. **OAuth PKCE flow** — how a user authorises a skill, handled by ephemeral in-memory `OAuthFlowState` and a `GET /oauth/callback` axum handler.
3. **Skill installation** — the concrete implementation for `skill-gmail`, `skill-github`, and `skill-google-calendar`, including scopes, token lifetimes, and refresh behaviour.

---

## Crate Note: `ring` and `aes-gcm` on Native Targets

The spec calls for `ring` and `aes-gcm`. Their roles are split deliberately:

- **`ring`** — used for HKDF key derivation (`ring::hkdf`). On native targets, `ring` compiles without restriction — all primitives (HKDF, HMAC, and the C-assembly-backed operations) work out of the box.
- **`aes-gcm`** — used for AES-256-GCM encryption/decryption. Pure Rust, security-audited by NCC Group. This is the primary encryption primitive.

Both crates work unrestricted on native targets. No special feature flags or target workarounds are needed.

---

## Phase 1 — Envelope Encryption with Two-Source Key Derivation

### Goal

Store encrypted OAuth tokens in `edgeclaw-server`'s SQLite (via `sqlx`) such that:

- A dump of the SQLite database reveals no usable secrets
- The encryption key is never written to any storage medium
- Each user's tokens are encrypted with a key unique to that user
- `edgeclaw-server` detects expired tokens and silently refreshes them before each skill call

---

### 1.1 — The Two-Source Key Problem

A single encryption key stored as an environment variable is insufficient alone: if an attacker gains access to the VPS filesystem (e.g. via a container escape or a compromised backup), all users' tokens would be decryptable in bulk if the key were co-located with the database. The solution is to require **two independent pieces of material** to derive any user's token encryption key:

**Source 1 — Deployment secret** (`TOKEN_MASTER_KEY`): A 256-bit random value stored as an environment variable in `.env` (or injected as a Docker secret). Set once per deployment. Never written to SQLite. Never logged. Rotatable (rotation invalidates all stored tokens — see 1.5).

**Source 2 — Per-user salt**: A 256-bit random value generated once per user on their first credential storage operation. Stored in plaintext in the `credentials` table. By itself it is meaningless — it only becomes useful when combined with `TOKEN_MASTER_KEY` via HKDF.

Neither source alone is sufficient to derive the key. An attacker who only has the SQLite file cannot derive keys (the master key lives in the environment, not on disk). An attacker who only has the environment variable cannot target a specific user's tokens without the corresponding salt from the database.

---

### 1.2 — Key Derivation

The per-credential encryption key is derived using HKDF-SHA256 (`ring::hkdf`):

```
encryption_key = HKDF-SHA256(
    ikm  = TOKEN_MASTER_KEY,      // 256-bit env var secret
    salt = user_salt,             // 256-bit random, stored in SQLite
    info = "edgeclaw-token-v1:{provider}"  // domain separation per provider
)
```

The `info` field binds the derived key to a specific provider (e.g. `"edgeclaw-token-v1:github"`). This means the key derived for GitHub cannot be used to decrypt a Gmail token, even if the user_salt were reused across providers — which it should not be, but this provides defence in depth.

The output is a 256-bit key, used directly as the AES-256-GCM key for that credential.

---

### 1.3 — Encryption and Storage

Encryption uses AES-256-GCM (`aes-gcm` crate). Each `encrypt()` call generates a fresh 96-bit random nonce. The nonce is prepended to the ciphertext before storage. The 16-byte GCM authentication tag is appended by the `aes-gcm` crate automatically and included in the stored blob.

Stored blob layout (all in the `access_token` / `refresh_token` columns):

```
[ nonce (12 bytes) | ciphertext | auth_tag (16 bytes) ]
```

The `credentials` table in `edgeclaw-server`'s SQLite (managed via `sqlx` migrations):

```sql
CREATE TABLE IF NOT EXISTS credentials (
    user_id           TEXT    NOT NULL REFERENCES users(id),
    skill_name        TEXT    NOT NULL,
    provider          TEXT    NOT NULL,   -- "github", "gmail", "google-calendar"
    access_token_enc  BLOB    NOT NULL,   -- nonce + ciphertext + tag
    refresh_token_enc BLOB,               -- nonce + ciphertext + tag, nullable
    expires_at        INTEGER,            -- unix ms, plaintext (not sensitive)
    scopes            TEXT    NOT NULL,   -- space-separated, plaintext for audit UI
    user_salt         BLOB    NOT NULL,   -- 32 random bytes, per credential
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    PRIMARY KEY (user_id, skill_name, provider)
);
```

The `user_id` column distinguishes credentials across users (since there is no per-user process boundary — all users share a single `edgeclaw-server` instance and SQLite database).

The `credential-store` crate exposes two operations:

- **`store(master_key, user_id, provider, plaintext_token, refresh_token, expires_at, scopes)`** — derives the key, encrypts both tokens with fresh nonces and salts, writes the row to SQLite.
- **`load(master_key, user_id, provider)`** — reads the row, derives the key from stored salt, decrypts both tokens, returns plaintext `Credential` struct. The plaintext values exist only in memory for the duration of the call.

The `master_key` argument is always read from `ServerConfig` (decoded from the `TOKEN_MASTER_KEY` env var at startup) — it is never written to SQLite or logged anywhere.

---

### 1.4 — Token Refresh in `edgeclaw-server`

Before dispatching any skill call, `edgeclaw-server` checks whether the credential for that skill is expired. This check is cheap: `expires_at` is stored in plaintext, so no decryption is needed just to check.

Refresh procedure:

1. Compare `expires_at` against current time. Add a 60-second buffer to avoid races at the boundary.
2. If not expired, decrypt the access token and pass it to the skill's MCP call.
3. If expired:
   a. Decrypt the refresh token.
   b. POST to the provider's token endpoint with the refresh token.
   c. Receive a new access token (and possibly a new refresh token).
   d. Re-encrypt and update the `credentials` row (new nonce, new `expires_at`).
   e. Proceed with the new access token.
4. If refresh fails (e.g. refresh token revoked by the user): mark the credential as invalid, notify the user via WebSocket, and skip the skill call with a user-facing error.

The credential check and refresh happen inside a **sqlx transaction** — the SELECT and UPDATE are atomic. If the process crashes mid-refresh, the partially completed update is rolled back, leaving the previous credential intact. On the next request, the server detects the (still-expired) token and retries the refresh.

Refresh token handling per provider:

- **GitHub** — refresh tokens are not issued for OAuth Apps (access tokens do not expire). For GitHub Apps, tokens expire after 8 hours, refresh tokens after 6 months.
- **Gmail / Google Calendar** — access tokens expire after 1 hour. Refresh tokens are long-lived (until revoked). Google issues a new refresh token only if `access_type=offline` and `prompt=consent` were included in the authorisation request. Always request these.

---

### 1.5 — Key Rotation

If `TOKEN_MASTER_KEY` needs to be rotated (e.g. after a suspected secret exposure):

1. Generate a new master key and update the `.env` file (or Docker secret). Restart the container.
2. All existing encrypted tokens become undecryptable.
3. Each user who next triggers a skill call will receive a refresh failure, prompting them to re-authorise.
4. Alternatively, run a migration: load all credentials with the old key, re-encrypt with the new key, update rows in a transaction.

The rotation migration is a one-off admin invocation, not part of the hot path. It should be gated behind an admin-only endpoint (e.g. `POST /admin/rotate-keys`), authenticated with `ADMIN_TOKEN`.

---

### 1.6 — Phase 1 Crate Dependencies

```toml
# crates/credential-store/Cargo.toml
[dependencies]
aes-gcm     = "0.10"       # pure Rust AES-256-GCM
ring        = "0.17"       # HKDF-SHA256 key derivation
rand        = { version = "0.8", features = ["getrandom"] }
serde       = { version = "1", features = ["derive"] }
thiserror   = "1"
zeroize     = "1"          # zero plaintext buffers before drop
```

The `zeroize` crate ensures plaintext token bytes in memory are overwritten with zeros when the containing struct is dropped. Combined with Rust's ownership model, this minimises the window during which plaintext tokens exist in heap memory.

---

### 1.7 — Phase 1 Milestones

| Milestone | Description | Done When |
|---|---|---|
| M3.1.1 | `credential-store` crate compiles and unit tests pass | `cargo test -p credential-store` clean |
| M3.1.2 | HKDF derivation produces distinct keys for different providers and salts | Unit test with known vectors passes |
| M3.1.3 | AES-256-GCM round-trips plaintext through encrypt/decrypt | Property test: decrypt(encrypt(pt)) == pt for random pt |
| M3.1.4 | `store` and `load` persist and recover credentials via sqlx SQLite round-trip | Integration test with real SQLite round-trip |
| M3.1.5 | Tampered ciphertext or nonce causes decryption failure, not garbage output | Authentication tag rejection test passes |
| M3.1.6 | Token refresh detects expiry and calls provider token endpoint | Fixture-based refresh test with mock HTTP |
| M3.1.7 | Refresh failure marks credential invalid and surfaces error to user | Error propagation test passes |
| M3.1.8 | Plaintext buffers are zeroed on drop | `zeroize` integration confirmed in test |

---

## Phase 2 — OAuth PKCE Flow

### Goal

When a user asks EdgeClaw to connect a service (e.g. "connect my GitHub account"), the agent initiates a standard OAuth 2.0 PKCE flow entirely within the `edgeclaw-server` process. The flow uses two components:

- **`OAuthFlowState`** — an ephemeral in-memory struct that holds PKCE state for the duration of the authorisation dance. Removed from the map on completion or expiry.
- **`GET /oauth/callback`** — an axum handler that receives the OAuth redirect, looks up the flow state by nonce, completes the token exchange, and stores the encrypted credential.

No OAuth state passes through the user's browser or Telegram client as anything other than an opaque, unguessable nonce.

---

### 2.1 — PKCE Primer

PKCE (Proof Key for Code Exchange, RFC 7636) protects the authorisation code flow against code interception. The client generates a secret `code_verifier` before redirecting the user, derives a `code_challenge` from it, and proves possession of the verifier when exchanging the code for tokens. An intercepted authorisation code is useless without the verifier.

```
code_verifier  = 32 random bytes, base64url-encoded (stored in OAuthFlowState)
code_challenge = BASE64URL(SHA256(code_verifier))  (sent in the redirect URL)
```

When the authorisation code arrives at the callback, the `/oauth/callback` handler retrieves the `code_verifier` from the in-memory flow state and includes it in the token exchange POST. The provider verifies that `SHA256(code_verifier) == code_challenge` from the original request.

---

### 2.2 — `OAuthFlowState` — Ephemeral In-Memory PKCE State

`OAuthFlowState` is held in an `Arc<Mutex<HashMap<String, OAuthFlowState>>>` on `AppState`. The HashMap key is the cryptographically random nonce. The entire lifetime of an entry is bounded by the OAuth flow — typically minutes. It holds only what is needed to complete the flow and no more.

```rust
pub struct OAuthFlowState {
    pub user_id: String,
    pub skill_name: String,
    pub provider: String,
    pub code_verifier: String,   // plaintext, in-flight secret
    pub scopes: String,
    pub expires_at: u64,         // unix ms; short TTL (~10 minutes)
    pub created_at: u64,
}
```

No SQLite is used for flow state — it is ephemeral and in-memory. If the server restarts mid-flow, the user simply re-initiates the connection (a minor inconvenience for a flow that takes seconds).

The flow state map supports two logical operations:

- **`init(user_id, skill_name, provider, scopes)`** — called by the agent turn handler to initialise the flow. Generates and stores the `code_verifier` as a new entry in the map. Returns the `code_challenge` and the nonce (used as the `state` parameter and lookup key).
- **`complete(nonce, code)`** — called by the `/oauth/callback` handler after receiving the redirect. Validates the nonce, removes the entry from the map (consuming it), retrieves the `code_verifier`, exchanges the authorisation code for tokens via the provider's token endpoint, and writes the encrypted credential to SQLite via the credential store.

A background `tokio::spawn` cleanup task runs every 60 seconds, iterating the map and removing any entries where `expires_at < now()`. This prevents orphaned flow state from accumulating if users never complete the authorisation.

---

### 2.3 — Full PKCE Flow

```
User (via Telegram/WebSocket):
  "Connect my GitHub account"

edgeclaw-server (agent turn handler):
  1. Generate nonce = random 16 bytes, base64url-encoded
  2. Create OAuthFlowState entry in the in-memory map keyed by nonce
  3. Generate code_verifier, compute code_challenge = BASE64URL(SHA256(code_verifier))
  4. Store { user_id, skill_name: "skill-github", provider: "github",
             code_verifier, scopes, expires_at: now + 10min } in the map
  5. Build the GitHub authorisation URL:
       https://github.com/login/oauth/authorize
         ?client_id={GITHUB_CLIENT_ID}
         &redirect_uri=https://edgeclaw.example.com/oauth/callback
         &scope=repo,user:email
         &state={nonce}
         &code_challenge={code_challenge}
         &code_challenge_method=S256
  6. Reply to user:
       "Tap this link to connect GitHub (expires in 10 minutes):
        https://edgeclaw.example.com/oauth/start/{nonce}"
       [Link opens in browser / Telegram opens web view]

User:
  Taps link -> redirected to GitHub consent screen -> approves scopes

GitHub:
  Redirects to: https://edgeclaw.example.com/oauth/callback
                  ?code={auth_code}&state={nonce}

GET /oauth/callback (axum handler):
  1. Parse code and state from query params
  2. Validate state is a well-formed nonce (not obviously malicious)
  3. Remove the OAuthFlowState entry from the map by nonce (consuming it)
  4. If no entry found (expired or already used), return 400 error page
  5. Check expires_at — reject if expired
  6. POST to https://github.com/login/oauth/access_token:
       { client_id, client_secret, code, redirect_uri, code_verifier }
  7. Receive { access_token, refresh_token, expires_in, scope }
  8. Encrypt and store credential in SQLite via credential-store:
       store(master_key, user_id, provider, access_token, refresh_token, expires_at, scopes)
  9. Notify user via WebSocket: "GitHub connected (scopes: repo, user:email)"
  10. Return 200 with a success page ("GitHub connected. You can close this tab.")
```

---

### 2.4 — Security Properties of the PKCE Flow

| Property | How it is achieved |
|---|---|
| Authorisation code cannot be replayed | `code_verifier` is single-use; entry removed from map after first `complete` call |
| Authorisation code interception is useless | Without `code_verifier`, intercepted `code` cannot be exchanged for tokens |
| `state` parameter forgery is not useful | `state` is a random nonce; a forged nonce finds no entry in the flow state map |
| Flow expiry prevents orphaned state | Background cleanup task removes expired entries every 60 seconds |
| Client secret never exposed to browser | All token exchange happens server-side in the `/oauth/callback` handler; the browser only handles redirect URLs |
| Provider tokens never transit the network in plaintext after exchange | The callback handler encrypts tokens directly via the credential store before any SQL write |
| CSRF via `state` parameter | `state` nonce is unguessable (128-bit random); browser never has a session that could be hijacked |

---

### 2.5 — Environment Variables (Phase 2)

The following environment variables are added to `.env` for OAuth support:

```bash
# .env (agent — never committed)

# Provider client IDs (non-secret, but kept in env for configurability)
GITHUB_CLIENT_ID=...
GOOGLE_CLIENT_ID=...

# Provider client secrets
GITHUB_CLIENT_SECRET=...
GOOGLE_CLIENT_SECRET=...

# Already present from Phase 1:
# TOKEN_MASTER_KEY=<32 random bytes, base64-encoded>
```

Alternatively, provider client secrets can be split into `.env.skills` if skills handle their own token exchange, or kept in the main `.env` if the server handles all OAuth flows centrally (which is the recommended approach for PKCE, since the server holds the `code_verifier`).

---

### 2.6 — Phase 2 Milestones

| Milestone | Description | Done When |
|---|---|---|
| M3.2.1 | `OAuthFlowState` created in memory, generates PKCE pair, stores state | Unit test: code_challenge == BASE64URL(SHA256(code_verifier)) |
| M3.2.2 | `/oauth/callback` handler routes callback to correct flow state by nonce | Integration test with test server |
| M3.2.3 | `complete()` exchanges code with mock token endpoint | Fixture-based token exchange test |
| M3.2.4 | Tokens written encrypted to SQLite via credential store | End-to-end: credential readable after flow |
| M3.2.5 | Cleanup task removes expired flows from the in-memory map | Test: expired entry absent after cleanup runs |
| M3.2.6 | User notified via WebSocket on successful connection | WS message received after `complete` |
| M3.2.7 | Expired flow rejected cleanly | `complete` after `expires_at` returns error, no token written |
| M3.2.8 | Full PKCE round-trip against real GitHub OAuth sandbox | Manual end-to-end on `cargo run` or `docker compose up` |

---

## Phase 3 — Skill Installation

### Overview

Each skill is a separate Rust HTTP service that implements the MCP protocol over HTTP. It receives a short-lived OAuth access token on each tool call (passed by `edgeclaw-server` from the decrypted credential), calls the upstream API on behalf of the user, and returns a `ToolCallResult`. Skills never store credentials — they are stateless with respect to tokens.

The `edgeclaw-server` always:
1. Checks and refreshes the credential if needed (Phase 1)
2. Decrypts the access token
3. Passes it as a `Bearer` token in the `Authorization` header of the MCP tool call to the skill service
4. Discards the plaintext token after the request

---

### 3.1 — `skill-gmail`

**Provider:** Google (Gmail API v1)
**OAuth endpoint:** `https://accounts.google.com/o/oauth2/v2/auth`
**Token endpoint:** `https://oauth2.googleapis.com/token`
**Access token lifetime:** 1 hour
**Refresh tokens:** Yes, long-lived (until revoked or app permission removed)

**Required scopes:**

```
https://www.googleapis.com/auth/gmail.readonly        -- read messages and labels
https://www.googleapis.com/auth/gmail.send            -- send messages
https://www.googleapis.com/auth/gmail.modify          -- archive, label, trash
https://www.googleapis.com/auth/gmail.labels          -- manage labels
```

Request `offline` access and `prompt=consent` to guarantee a refresh token is issued.

**MCP tools exposed:**

| Tool | Description | Destructive |
|---|---|---|
| `gmail_list_messages` | List messages matching a query (sender, subject, label, date range) | No |
| `gmail_get_message` | Fetch full message body and headers by ID | No |
| `gmail_search` | Search messages using Gmail query syntax | No |
| `gmail_send` | Send a new email | **Yes — requires approval** |
| `gmail_reply` | Reply to an existing thread | **Yes — requires approval** |
| `gmail_archive` | Archive messages by ID or query | **Yes — requires approval** |
| `gmail_label` | Apply or remove labels | No |
| `gmail_get_labels` | List all labels | No |

**Refresh strategy:** Google access tokens expire in exactly 3600 seconds. `edgeclaw-server` checks `expires_at` with a 60-second buffer. On expiry, POST to the token endpoint with `grant_type=refresh_token` and the decrypted refresh token. Google may return a new refresh token; if it does, update the stored `refresh_token_enc`. If Google returns `invalid_grant`, the refresh token has been revoked — mark the credential invalid and prompt the user to re-authorise.

**Installation flow:**

```
User:  "/add-skill gmail"
Agent: "To connect Gmail I'll need the following permissions:
        - Read your messages and labels
        - Send emails (only when you approve each one)
        - Archive and label messages

        Tap this link to connect (expires in 10 minutes):
        https://edgeclaw.example.com/oauth/start/{nonce}"
```

The agent lists scopes in plain language, not technical scope strings, before generating the link. Destructive tools (`gmail_send`, `gmail_reply`, `gmail_archive`) trigger the human-in-the-loop approval flow in `edgeclaw-server` before execution.

---

### 3.2 — `skill-github`

**Provider:** GitHub
**OAuth endpoint:** `https://github.com/login/oauth/authorize`
**Token endpoint:** `https://github.com/login/oauth/access_token`
**Access token lifetime:** No expiry for OAuth Apps. GitHub Apps tokens expire after 8 hours.
**Refresh tokens:** Only for GitHub Apps (not OAuth Apps)

> **Implementation note:** Use a GitHub **OAuth App** for the prototype — simpler setup, no token expiry to manage. GitHub Apps provide finer permission scopes and installation-level access but require additional setup. The `expires_at` column should store `NULL` for OAuth App tokens.

**Required scopes:**

```
repo          -- full repo access (read + write); consider repo:read for read-only
user:email    -- read verified email address
read:org      -- read org membership (optional, for org-scoped queries)
```

**MCP tools exposed:**

| Tool | Description | Destructive |
|---|---|---|
| `github_list_repos` | List repos for authenticated user or an org | No |
| `github_get_repo` | Get repo metadata, description, topics, stats | No |
| `github_list_issues` | List issues with filter (state, label, assignee, milestone) | No |
| `github_get_issue` | Get issue body and comments | No |
| `github_create_issue` | Create a new issue | **Yes — requires approval** |
| `github_comment_issue` | Add a comment to an issue | **Yes — requires approval** |
| `github_list_prs` | List pull requests with filter | No |
| `github_get_pr` | Get PR diff summary and review status | No |
| `github_list_commits` | List commits for a branch | No |
| `github_search_code` | Search code across repos using GitHub code search | No |
| `github_search_issues` | Search issues and PRs across repos | No |

**Refresh strategy:** For OAuth Apps, `expires_at` is `NULL` — no refresh needed. For GitHub Apps, the refresh flow mirrors Google's: POST to `https://github.com/login/oauth/access_token` with `grant_type=refresh_token`.

**Rate limiting:** GitHub REST API allows 5,000 requests/hour for authenticated users. The skill should surface `X-RateLimit-Remaining` in tool error responses when the limit is low, so the agent can inform the user rather than silently failing.

**Installation flow:**

```
User:  "/add-skill github"
Agent: "To connect GitHub I'll need:
        - Read access to your repos, issues, and pull requests
        - Ability to create issues and add comments
          (I'll ask for your approval before doing either)

        Tap this link to connect (expires in 10 minutes):
        https://edgeclaw.example.com/oauth/start/{nonce}"
```

---

### 3.3 — `skill-google-calendar`

**Provider:** Google (Calendar API v3)
**OAuth endpoint:** `https://accounts.google.com/o/oauth2/v2/auth`
**Token endpoint:** `https://oauth2.googleapis.com/token`
**Access token lifetime:** 1 hour
**Refresh tokens:** Yes (same Google OAuth infrastructure as Gmail)

**Required scopes:**

```
https://www.googleapis.com/auth/calendar.readonly     -- read events and calendars
https://www.googleapis.com/auth/calendar.events       -- create, edit, delete events
```

Request `offline` access and `prompt=consent`. If the user has already connected Gmail, Google may share the same `refresh_token` across scopes within the same OAuth client — but EdgeClaw stores each provider separately in the `credentials` table to avoid cross-skill coupling. The user may need to grant Calendar permissions separately even if Gmail is already connected.

**MCP tools exposed:**

| Tool | Description | Destructive |
|---|---|---|
| `calendar_list_calendars` | List all calendars the user has access to | No |
| `calendar_list_events` | List events in a calendar within a time range | No |
| `calendar_get_event` | Get a specific event by ID | No |
| `calendar_search_events` | Full-text search across events | No |
| `calendar_create_event` | Create a new event | **Yes — requires approval** |
| `calendar_update_event` | Update an existing event (time, title, attendees) | **Yes — requires approval** |
| `calendar_delete_event` | Delete an event | **Yes — requires approval** |
| `calendar_find_free_slots` | Given a duration and a date range, return available free slots across calendars | No |

The `calendar_find_free_slots` tool is a computed tool — the skill calls `freebusy.query` on the Google Calendar API (which returns busy blocks) and inverts the result to produce free slots. This is the most useful agent-native tool: the LLM can plan meetings without ever exposing the user's raw calendar data.

**Refresh strategy:** Identical to `skill-gmail` — 1-hour expiry, refresh on 60-second buffer, `invalid_grant` triggers re-authorisation.

**Installation flow:**

```
User:  "/add-skill google-calendar"
Agent: "To connect Google Calendar I'll need:
        - Read access to your calendars and events
        - Ability to create, edit, and delete events
          (I'll ask for your approval before any changes)

        Tap this link to connect (expires in 10 minutes):
        https://edgeclaw.example.com/oauth/start/{nonce}"
```

---

### 3.4 — Shared Skill Architecture

All three skills follow the same structural pattern:

```
+-----------------------------------------------------------+
|  edgeclaw-server                                          |
|                                                           |
|  Before each tool call:                                   |
|    1. Check expires_at (plaintext) -- refresh if needed   |
|    2. Derive key: HKDF(master_key, user_salt, info)       |
|    3. Decrypt access token via AES-256-GCM                |
|    4. Include as Bearer token in MCP request header       |
|    5. Call skill service via reqwest HTTP                  |
|    6. Plaintext token dropped (zeroized)                  |
+---------------------------+-------------------------------+
                            | MCP over HTTP
                            | Authorization: Bearer {access_token}
            +---------------+---------------+
            v               v               v
  +--------------+ +--------------+ +------------------+
  | skill-gmail  | |skill-github  | |skill-google-cal  |
  |              | |              | |                   |
  | Stateless    | | Stateless    | | Stateless         |
  | Rust service | | Rust service | | Rust service      |
  |              | |              | |                   |
  | Calls Gmail  | | Calls GitHub | | Calls Calendar    |
  | API with     | | API with     | | API with          |
  | Bearer token | | Bearer token | | Bearer token      |
  |              | |              | |                   |
  | No token     | | No token     | | No token          |
  | storage      | | storage      | | storage           |
  +--------------+ +--------------+ +------------------+
```

Each skill service exposes only its MCP endpoint. It receives the access token per-call, uses it to call the upstream API, and returns a `ToolCallResult`. It has no persistent storage and no knowledge of the user's identity beyond what the token encodes.

The upstream API call uses the skill's environment variable for its **own** OAuth client secret (used in the token endpoint during the PKCE exchange), not the user's access token — that comes from `edgeclaw-server`.

---

### 3.5 — Credential Listing and Revocation

`edgeclaw-server` exposes a read-only credential inventory tool available to the agent:

**`credentials_list`** — returns skill name, provider, scopes, and `expires_at` for each stored credential. Does **not** decrypt or return any token material.

Users can revoke access via a conversational command:

```
User:  "Disconnect my GitHub account"
Agent: "Disconnected. I've removed your GitHub credentials from my storage.
        Note: to fully revoke access, visit github.com/settings/applications
        and remove EdgeClaw from your authorized apps."
```

On disconnect, `edgeclaw-server` deletes the row from the `credentials` table (within a sqlx transaction). The underlying token remains valid at the provider until it expires or the user revokes it there directly — EdgeClaw cannot invalidate the token on the provider's side without a separate revocation API call, which can be added as a Phase 4 enhancement.

---

### 3.6 — Phase 3 Milestones

| Milestone | Description | Done When |
|---|---|---|
| M3.3.1 | `skill-gmail` container starts and lists messages with a real token | Manual smoke test against Gmail API |
| M3.3.2 | `skill-gmail` destructive tools trigger approval flow in `edgeclaw-server` | Approval round-trip test via WebSocket |
| M3.3.3 | `skill-github` container starts and lists repos and issues | Manual smoke test against GitHub API |
| M3.3.4 | `skill-github` rate limit surfaced in tool error response | Rate limit header parsed and returned in ToolCallResult |
| M3.3.5 | `skill-google-calendar` container starts and lists events | Manual smoke test against Calendar API |
| M3.3.6 | `calendar_find_free_slots` returns correct free blocks | Unit test against known freebusy fixture |
| M3.3.7 | Token refresh works end-to-end for Google skills (1-hour expiry) | Integration test: expire a token artificially, confirm refresh |
| M3.3.8 | `credentials_list` tool returns inventory without token material | Confirmed no token bytes in response |
| M3.3.9 | Disconnect removes credential row and confirms to user | SQLite row absent after disconnect command |
| M3.3.10 | Full multi-skill scenario: agent reads GitHub issues, creates calendar event | End-to-end demo with real APIs |

---

## Security Model Summary

| Threat | Mitigation |
|---|---|
| SQLite dump exposes tokens | AES-256-GCM encryption; key never stored in database |
| Environment variable leaked | Tokens still protected by per-user salt; attacker needs both env var and database |
| Skill service compromised | Skill never holds refresh tokens; access tokens are short-lived |
| Malicious skill reads another skill's token | `edgeclaw-server` only decrypts the token for the specific skill being called; skills communicate via HTTP only |
| OAuth code interception | PKCE: intercepted code useless without `code_verifier` held in server memory |
| CSRF on OAuth callback | Unguessable 128-bit nonce as `state`; forged state finds no entry in flow state map |
| In-memory flow state leaks | Entries are short-lived (10-minute TTL); cleanup task removes expired entries every 60 seconds; server restart clears all in-flight flows |
| Replay of used authorisation code | Flow state entry removed from map on `complete` — second call finds no entry |
| Token reuse after user disconnects | Credential row deleted from SQLite; subsequent calls fail cleanly |
| Runaway agent sends emails or creates events | Destructive tools require explicit human-in-the-loop approval before execution |
| Bulk token exfiltration via compromised server | Plaintext tokens exist only during a single request turn; `zeroize` clears heap on drop |
