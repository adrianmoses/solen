# EdgeClaw — Credential Management Specification

> Per-user OAuth credential storage, PKCE-based authorisation flows, and skill installation for Gmail, GitHub, and Google Calendar.

---

## Overview

OpenClaw and NanoClaw treat credential management as an afterthought — tokens land in plaintext config files or unencrypted storage, and the trust boundary between skills is the application layer rather than the cryptographic layer. EdgeClaw takes a different approach: tokens are **never stored in plaintext anywhere**, the encryption key **never exists in storage**, and each skill's authorisation is **scoped and auditable**.

This spec covers three sequential implementation phases:

1. **Envelope encryption** — how per-user OAuth tokens are stored encrypted in `AgentDO`'s SQLite, with a two-source key derivation scheme and automatic token refresh.
2. **OAuth PKCE flow** — how a user authorises a skill, handled by an ephemeral `OAuthDO` and a stateless `OAuthCallbackWorker`.
3. **Skill installation** — the concrete implementation for `skill-gmail`, `skill-github`, and `skill-google-calendar`, including scopes, token lifetimes, and refresh behaviour.

---

## Crate Note: `ring` vs `aes-gcm` on WASM

The spec calls for `ring` and `aes-gcm`. Their roles are split deliberately:

- **`ring`** — used for HKDF key derivation (`ring::hkdf`). `ring` has partial `wasm32-unknown-unknown` support; its HMAC and HKDF primitives compile cleanly. Its asymmetric crypto and some C-assembly-backed operations do not. Verify HKDF compiles for your target before relying on it; if it does not, `hkdf` + `hmac` from the RustCrypto family are a pure-Rust drop-in.
- **`aes-gcm`** — used for AES-256-GCM encryption/decryption. Pure Rust, no C FFI, compiles cleanly to `wasm32-unknown-unknown`. Security-audited by NCC Group. This is the primary encryption primitive.

---

## Phase 1 — Envelope Encryption with Two-Source Key Derivation

### Goal

Store encrypted OAuth tokens in `AgentDO`'s SQLite such that:

- A dump of the SQLite database reveals no usable secrets
- The encryption key is never written to any storage medium
- Each user's tokens are encrypted with a key unique to that user
- `AgentDO` detects expired tokens and silently refreshes them before each skill call

---

### 1.1 — The Two-Source Key Problem

A single encryption key stored as a Worker secret is insufficient alone: if a Cloudflare employee or a compromised deployment pipeline could read Worker secrets, all users' tokens would be decryptable in bulk. The solution is to require **two independent pieces of material** to derive any user's token encryption key:

**Source 1 — Deployment secret** (`TOKEN_MASTER_KEY`): A 256-bit random value stored as a Worker secret binding. Set once per deployment via `wrangler secret put`. Never written to SQLite. Never logged. Rotatable (rotation invalidates all stored tokens — see §1.5).

**Source 2 — Per-user salt**: A 256-bit random value generated once per user on their first credential storage operation. Stored in plaintext in the `credentials` table. By itself it is meaningless — it only becomes useful when combined with `TOKEN_MASTER_KEY` via HKDF.

Neither source alone is sufficient to derive the key. An attacker who only has the SQLite dump cannot derive keys. An attacker who only has the Worker secret cannot target a specific user's tokens without the corresponding salt.

---

### 1.2 — Key Derivation

The per-credential encryption key is derived using HKDF-SHA256 (`ring::hkdf`):

```
encryption_key = HKDF-SHA256(
    ikm  = TOKEN_MASTER_KEY,      // 256-bit Worker secret
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

The `AgentDO` `credentials` table:

```sql
CREATE TABLE IF NOT EXISTS credentials (
    skill_name        TEXT    NOT NULL,
    provider          TEXT    NOT NULL,   -- "github", "gmail", "google-calendar"
    access_token_enc  BLOB    NOT NULL,   -- nonce + ciphertext + tag
    refresh_token_enc BLOB,               -- nonce + ciphertext + tag, nullable
    expires_at        INTEGER,            -- unix ms, plaintext (not sensitive)
    scopes            TEXT    NOT NULL,   -- space-separated, plaintext for audit UI
    user_salt         BLOB    NOT NULL,   -- 32 random bytes, per credential
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    PRIMARY KEY (skill_name, provider)
);
```

The `credential-store` crate (part of `edgeclaw-worker`) exposes two operations:

- **`store(master_key, provider, plaintext_token, refresh_token, expires_at, scopes)`** — derives the key, encrypts both tokens with fresh nonces and salts, writes the row to SQLite.
- **`load(master_key, provider)`** — reads the row, derives the key from stored salt, decrypts both tokens, returns plaintext `Credential` struct. The plaintext values exist only in memory for the duration of the call.

The `master_key` argument is always passed from `Env` at the start of each `AgentDO` request — it is never cached in any field or written anywhere.

---

### 1.4 — Token Refresh in `AgentDO`

Before dispatching any skill call, `AgentDO` checks whether the credential for that skill is expired. This check is cheap: `expires_at` is stored in plaintext, so no decryption is needed just to check.

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

This entire flow happens inside a single `AgentDO` request, within the platform's single-threaded execution guarantee. There are no race conditions on the credential row.

Refresh token handling per provider:

- **GitHub** — refresh tokens are not issued for OAuth Apps (access tokens do not expire). For GitHub Apps, tokens expire after 8 hours, refresh tokens after 6 months.
- **Gmail / Google Calendar** — access tokens expire after 1 hour. Refresh tokens are long-lived (until revoked). Google issues a new refresh token only if `access_type=offline` and `prompt=consent` were included in the authorisation request. Always request these.

---

### 1.5 — Key Rotation

If `TOKEN_MASTER_KEY` needs to be rotated (e.g. after a suspected secret exposure):

1. Generate a new master key and set it as the Worker secret.
2. All existing encrypted tokens become undecryptable.
3. Each user who next triggers a skill call will receive a refresh failure, prompting them to re-authorise.
4. Alternatively, run a migration: load all credentials with the old key, re-encrypt with the new key, update rows in a transaction.

The rotation migration is a one-off Worker invocation, not part of the hot path. It should be gated behind an admin-only endpoint on a separate Worker binding, not exposed on `AgentDO`.

---

### 1.6 — Phase 1 Crate Dependencies

```toml
# crates/credential-store/Cargo.toml
[dependencies]
aes-gcm     = "0.10"       # pure Rust, wasm32-unknown-unknown compatible
ring        = "0.17"       # for HKDF — verify WASM target builds in CI
rand        = { version = "0.8", features = ["getrandom"] }
getrandom   = { version = "0.2", features = ["js"] }  # required for rand on wasm
serde       = { version = "1", features = ["derive"] }
thiserror   = "1"
zeroize     = "1"          # zero plaintext buffers before drop
```

The `zeroize` crate ensures plaintext token bytes in memory are overwritten with zeros when the containing struct is dropped. Combined with Rust's ownership model, this minimises the window during which plaintext tokens exist in the WASM heap.

---

### 1.7 — Phase 1 Milestones

| Milestone | Description | Done When |
|---|---|---|
| M3.1.1 | `credential-store` crate compiles to `wasm32-unknown-unknown` | `cargo build --target wasm32-unknown-unknown` clean |
| M3.1.2 | HKDF derivation produces distinct keys for different providers and salts | Unit test with known vectors passes |
| M3.1.3 | AES-256-GCM round-trips plaintext through encrypt/decrypt | Property test: decrypt(encrypt(pt)) == pt for random pt |
| M3.1.4 | `store` and `load` persist and recover credentials via Miniflare SQLite | Integration test with real SQLite round-trip |
| M3.1.5 | Tampered ciphertext or nonce causes decryption failure, not garbage output | Authentication tag rejection test passes |
| M3.1.6 | Token refresh detects expiry and calls provider token endpoint | Fixture-based refresh test with mock HTTP |
| M3.1.7 | Refresh failure marks credential invalid and surfaces error to user | Error propagation test passes |
| M3.1.8 | Plaintext buffers are zeroed on drop | `zeroize` integration confirmed in test |

---

## Phase 2 — OAuth PKCE Flow

### Goal

When a user asks EdgeClaw to connect a service (e.g. "connect my GitHub account"), the agent initiates a standard OAuth 2.0 PKCE flow entirely within the Cloudflare Workers platform. The flow uses two components:

- **`OAuthDO`** — an ephemeral Durable Object that holds PKCE state for the duration of the authorisation dance. Self-destructs on completion or expiry.
- **`OAuthCallbackWorker`** — a stateless `#[event(fetch)]` handler that receives the OAuth redirect, looks up the `OAuthDO`, completes the token exchange, and stores the encrypted credential in `AgentDO`.

No OAuth state passes through the user's browser or Telegram client as anything other than an opaque, unguessable nonce.

---

### 2.1 — PKCE Primer

PKCE (Proof Key for Code Exchange, RFC 7636) protects the authorisation code flow against code interception. The client generates a secret `code_verifier` before redirecting the user, derives a `code_challenge` from it, and proves possession of the verifier when exchanging the code for tokens. An intercepted authorisation code is useless without the verifier.

```
code_verifier  = 32 random bytes, base64url-encoded (stored in OAuthDO)
code_challenge = BASE64URL(SHA256(code_verifier))  (sent in the redirect URL)
```

When the authorisation code arrives at the callback, the `OAuthCallbackWorker` retrieves the `code_verifier` from `OAuthDO` and includes it in the token exchange POST. The provider verifies that `SHA256(code_verifier) == code_challenge` from the original request.

---

### 2.2 — `OAuthDO` — Ephemeral PKCE State

`OAuthDO` is a SQLite-backed Durable Object whose identity is a cryptographically random nonce: `oauth:{nonce}`. Its entire lifetime is bounded by the OAuth flow — typically minutes. It holds only what is needed to complete the flow and no more.

SQLite schema (initialised on first access):

```sql
CREATE TABLE IF NOT EXISTS oauth_state (
    user_id       TEXT    NOT NULL,
    skill_name    TEXT    NOT NULL,
    provider      TEXT    NOT NULL,
    code_verifier TEXT    NOT NULL,  -- plaintext, in-flight secret
    scopes        TEXT    NOT NULL,
    expires_at    INTEGER NOT NULL,  -- unix ms; short TTL (~10 minutes)
    created_at    INTEGER NOT NULL
);
```

`OAuthDO` exposes two RPC methods (via `Stub::fetch()`):

- **`POST /init`** — called by `AgentDO` to initialise the flow. Generates and stores the `code_verifier`. Returns the `code_challenge` and a signed `state` parameter (the nonce, usable as a lookup key).
- **`POST /complete`** — called by `OAuthCallbackWorker` after receiving the redirect. Validates the `state`, retrieves the `code_verifier`, exchanges the authorisation code for tokens via the provider's token endpoint, writes the encrypted credential to `AgentDO` via stub, then deletes its own SQLite storage and ceases to exist.

`OAuthDO` also registers a Durable Object alarm for its `expires_at` timestamp. If the user never completes the flow, the alarm fires and the DO deletes itself, preventing orphaned flow state from accumulating.

---

### 2.3 — Full PKCE Flow

```
User (via Telegram/WebSocket):
  "Connect my GitHub account"

AgentDO:
  1. Generate nonce = random 16 bytes, base64url-encoded
  2. Get OAuthDO stub: id_from_name("oauth:{nonce}")
  3. POST /init to OAuthDO:
       { user_id, skill_name: "skill-github", provider: "github", scopes: [...] }
  4. OAuthDO responds with:
       { code_challenge, code_challenge_method: "S256", state: nonce }
  5. AgentDO builds the GitHub authorisation URL:
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
  Taps link → redirected to GitHub consent screen → approves scopes

GitHub:
  Redirects to: https://edgeclaw.example.com/oauth/callback
                  ?code={auth_code}&state={nonce}

OAuthCallbackWorker (#[event(fetch)]):
  1. Parse code and state from query params
  2. Validate state is a well-formed nonce (not obviously malicious)
  3. Get OAuthDO stub: id_from_name("oauth:{state}")
  4. POST /complete to OAuthDO: { code }
  5. Return 200 with a success page ("GitHub connected. You can close this tab.")

OAuthDO.complete(code):
  1. Load code_verifier, user_id, scopes from SQLite
  2. Check expires_at — reject if expired
  3. POST to https://github.com/login/oauth/access_token:
       { client_id, client_secret, code, redirect_uri, code_verifier }
  4. Receive { access_token, refresh_token, expires_in, scope }
  5. Get AgentDO stub: id_from_name("agent:{user_id}")
  6. POST /credentials/store to AgentDO:
       { skill_name, provider, access_token, refresh_token, expires_at, scopes }
  7. AgentDO encrypts and persists (Phase 1 flow)
  8. AgentDO notifies user via WebSocket: "GitHub connected ✓ (scopes: repo, user:email)"
  9. OAuthDO: DELETE own SQLite storage → DO ceases to exist
```

---

### 2.4 — Security Properties of the PKCE Flow

| Property | How it is achieved |
|---|---|
| Authorisation code cannot be replayed | `code_verifier` in `OAuthDO` is single-use; DO deletes itself after first `complete` call |
| Authorisation code interception is useless | Without `code_verifier`, intercepted `code` cannot be exchanged for tokens |
| `state` parameter forgery is not useful | `state` is a random nonce; a forged nonce finds no `OAuthDO` instance |
| Flow expiry prevents orphaned state | `OAuthDO` alarm fires at `expires_at`; DO deletes itself and any pending flow |
| Client secret never exposed to browser | All token exchange happens server-side in `OAuthDO`; the browser only handles redirect URLs |
| Provider tokens never transit the network in plaintext after exchange | `OAuthDO` writes directly to `AgentDO` via DO-to-DO stub call; tokens are encrypted before any SQL write |
| CSRF via `state` parameter | `state` nonce is unguessable (128-bit random); browser never has a session that could be hijacked |

---

### 2.5 — `wrangler.toml` additions (Phase 2)

```toml
# OAuthDO — ephemeral, short-lived
[[durable_objects.bindings]]
name = "OAUTH_DO"
class_name = "OAuthDo"

[[migrations]]
tag = "v2"
new_sqlite_classes = ["OAuthDo"]

# Provider client IDs (non-secret, safe as vars)
[vars]
GITHUB_CLIENT_ID         = "..."
GOOGLE_CLIENT_ID         = "..."

# Provider client secrets — set via: wrangler secret put
# GITHUB_CLIENT_SECRET
# GOOGLE_CLIENT_SECRET
# TOKEN_MASTER_KEY         (32 random bytes, base64-encoded)
```

---

### 2.6 — Phase 2 Milestones

| Milestone | Description | Done When |
|---|---|---|
| M3.2.1 | `OAuthDO` initialises, generates PKCE pair, stores state | Unit test: code_challenge == BASE64URL(SHA256(code_verifier)) |
| M3.2.2 | `OAuthCallbackWorker` routes callback to correct `OAuthDO` by nonce | Integration test with Miniflare |
| M3.2.3 | `OAuthDO.complete()` exchanges code with mock token endpoint | Fixture-based token exchange test |
| M3.2.4 | Tokens written encrypted to `AgentDO` via DO stub | End-to-end: credential readable after flow in Miniflare |
| M3.2.5 | `OAuthDO` alarm fires at `expires_at` and DO self-destructs | Alarm test: DO storage empty after alarm |
| M3.2.6 | User notified via WebSocket on successful connection | WS message received after `complete` in Miniflare |
| M3.2.7 | Expired flow rejected cleanly | `complete` after `expires_at` returns error, no token written |
| M3.2.8 | Full PKCE round-trip against real GitHub OAuth sandbox | Manual end-to-end on `wrangler dev` |

---

## Phase 3 — Skill Installation

### Overview

Each skill is a separate Rust Worker that implements the MCP protocol over HTTP. It receives a short-lived OAuth access token on each tool call (passed by `AgentDO` from the decrypted credential), calls the upstream API on behalf of the user, and returns a `ToolCallResult`. Skills never store credentials — they are stateless with respect to tokens.

The `AgentDO` always:
1. Checks and refreshes the credential if needed (Phase 1)
2. Decrypts the access token
3. Passes it as a `Bearer` token in the `Authorization` header of the MCP tool call to the skill Worker
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

**Refresh strategy:** Google access tokens expire in exactly 3600 seconds. `AgentDO` checks `expires_at` with a 60-second buffer. On expiry, POST to the token endpoint with `grant_type=refresh_token` and the decrypted refresh token. Google may return a new refresh token; if it does, update the stored `refresh_token_enc`. If Google returns `invalid_grant`, the refresh token has been revoked — mark the credential invalid and prompt the user to re-authorise.

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

The agent lists scopes in plain language, not technical scope strings, before generating the link. Destructive tools (`gmail_send`, `gmail_reply`, `gmail_archive`) trigger the human-in-the-loop approval flow in `AgentDO` before execution.

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
┌─────────────────────────────────────────────────────────┐
│  AgentDO                                                │
│                                                         │
│  Before each tool call:                                 │
│    1. Check expires_at (plaintext) — refresh if needed  │
│    2. Derive key: HKDF(master_key, user_salt, info)     │
│    3. Decrypt access token via AES-256-GCM              │
│    4. Include as Bearer token in MCP request header     │
│    5. Call skill Worker via service binding             │
│    6. Plaintext token dropped (zeroized)                │
└────────────────────────┬────────────────────────────────┘
                         │ MCP over HTTP
                         │ Authorization: Bearer {access_token}
         ┌───────────────┼───────────────┐
         ▼               ▼               ▼
  ┌─────────────┐ ┌─────────────┐ ┌──────────────────┐
  │ skill-gmail │ │skill-github │ │skill-google-cal  │
  │             │ │             │ │                  │
  │ Stateless   │ │ Stateless   │ │ Stateless        │
  │ Rust Worker │ │ Rust Worker │ │ Rust Worker      │
  │             │ │             │ │                  │
  │ Calls Gmail │ │ Calls GitHub│ │ Calls Calendar   │
  │ API with    │ │ API with    │ │ API with         │
  │ Bearer token│ │ Bearer token│ │ Bearer token     │
  │             │ │             │ │                  │
  │ No token    │ │ No token    │ │ No token         │
  │ storage     │ │ storage     │ │ storage          │
  └─────────────┘ └─────────────┘ └──────────────────┘
```

Each skill Worker exposes only its MCP endpoint. It receives the access token per-call, uses it to call the upstream API, and returns a `ToolCallResult`. It has no persistent storage and no knowledge of the user's identity beyond what the token encodes.

The upstream API call uses the Worker secret binding for the skill's **own** OAuth client secret (used in the token endpoint during the PKCE exchange), not the user's access token — that comes from `AgentDO`.

---

### 3.5 — Credential Listing and Revocation

`AgentDO` exposes a read-only credential inventory tool available to the agent:

**`credentials_list`** — returns skill name, provider, scopes, and `expires_at` for each stored credential. Does **not** decrypt or return any token material.

Users can revoke access via a conversational command:

```
User:  "Disconnect my GitHub account"
Agent: "Disconnected. I've removed your GitHub credentials from my storage.
        Note: to fully revoke access, visit github.com/settings/applications
        and remove EdgeClaw from your authorized apps."
```

On disconnect, `AgentDO` deletes the row from the `credentials` table (ACID transaction). The underlying token remains valid at the provider until it expires or the user revokes it there directly — EdgeClaw cannot invalidate the token on the provider's side without a separate revocation API call, which can be added as a Phase 4 enhancement.

---

### 3.6 — Phase 3 Milestones

| Milestone | Description | Done When |
|---|---|---|
| M3.3.1 | `skill-gmail` Worker deploys and lists messages with a real token | Manual smoke test against Gmail API |
| M3.3.2 | `skill-gmail` destructive tools trigger approval flow in `AgentDO` | Approval round-trip test via WebSocket |
| M3.3.3 | `skill-github` Worker deploys and lists repos and issues | Manual smoke test against GitHub API |
| M3.3.4 | `skill-github` rate limit surfaced in tool error response | Rate limit header parsed and returned in ToolCallResult |
| M3.3.5 | `skill-google-calendar` Worker deploys and lists events | Manual smoke test against Calendar API |
| M3.3.6 | `calendar_find_free_slots` returns correct free blocks | Unit test against known freebusy fixture |
| M3.3.7 | Token refresh works end-to-end for Google skills (1-hour expiry) | Integration test: expire a token artificially, confirm refresh |
| M3.3.8 | `credentials_list` tool returns inventory without token material | Confirmed no token bytes in response |
| M3.3.9 | Disconnect removes credential row and confirms to user | SQLite row absent after disconnect command |
| M3.3.10 | Full multi-skill scenario: agent reads GitHub issues, creates calendar event | End-to-end demo with real APIs |

---

## Security Model Summary

| Threat | Mitigation |
|---|---|
| SQLite dump exposes tokens | AES-256-GCM encryption; key never stored |
| Worker secret leaked | Tokens still protected by per-user salt; attacker needs both sources |
| Skill Worker compromised | Skill never holds refresh tokens; access tokens are short-lived |
| Malicious skill reads another skill's token | `AgentDO` only decrypts the token for the specific skill being called; skills communicate via HTTP only |
| OAuth code interception | PKCE: intercepted code useless without `code_verifier` held in `OAuthDO` |
| CSRF on OAuth callback | Unguessable 128-bit nonce as `state`; forged state finds no `OAuthDO` |
| `OAuthDO` state leaks | Alarm-based self-destruction at `expires_at`; DO ceases to exist after flow |
| Replay of used authorisation code | `OAuthDO` deletes itself on `complete` — second call finds no DO |
| Token reuse after user disconnects | Credential row deleted from SQLite; subsequent calls fail cleanly |
| Runaway agent sends emails or creates events | Destructive tools require explicit human-in-the-loop approval before execution |
| Bulk token exfiltration via compromised `AgentDO` | Plaintext tokens exist only during a single request turn; `zeroize` clears heap on drop |
