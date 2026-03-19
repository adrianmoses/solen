# EdgeClaw Development Guide

## Build & Check Commands

```bash
# Check crates
cargo check -p agent-core
cargo check -p edgeclaw-server

# Run unit tests
cargo test -p agent-core

# Clippy
cargo clippy -p agent-core -- -D warnings
cargo clippy -p edgeclaw-server -- -D warnings

# Format
cargo fmt --all -- --check

# Run the server locally
cargo run -p edgeclaw-server

# Docker build and run
docker compose up --build
```

## Architecture Rules

- **agent-core** must have zero server framework dependency. Pure Rust domain logic, compiles to native targets.
- **edgeclaw-server** is the axum + sqlx host. It depends on `agent-core`, `mcp-client`, `skill-registry`, and `credential-store`.
- The `HttpBackend` trait in agent-core abstracts HTTP calls — server implements it with `reqwest`, tests use `MockHttpBackend`.
- Skills are isolated HTTP services deployed as Docker containers.

## Specs

- **`EDGECLAW_SPEC.md`** — canonical architecture spec (VPS/tokio/axum/sqlx)
- **`EDGECLAW_CREDENTIALS_SPEC.md`** — credential encryption, OAuth PKCE, skill installation
- **`EDGECLAW_TUI_SPEC.md`** — CLI setup wizard and management TUI
- **`docs/archive/`** — historical Cloudflare Workers/Durable Objects specs (pre-migration)

## Testing Patterns

- **Unit tests**: `MockHttpBackend` with `RefCell<VecDeque<Vec<u8>>>` for pre-recorded API responses. Fixtures in `tests/fixtures/`.
- **Integration tests**: Against a running `edgeclaw-server` instance or via Docker Compose.

## Deployment

```bash
# Local dev
cp .env.example .env  # fill in secrets
cargo run -p edgeclaw-server

# Production (Docker Compose on VPS)
docker compose up -d
```

Secrets are configured via environment variables in `.env` (never committed).
Model configurable via `DEFAULT_MODEL` env var.
