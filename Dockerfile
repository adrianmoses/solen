# DEPRECATED: Use docker/Dockerfile.server instead (cargo-chef cached builds, Alpine, non-root user).
# This file is kept for quick local builds only.
FROM rust:1.83-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Build release binary
RUN cargo build --release -p edgeclaw-server

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/edgeclaw-server /usr/local/bin/edgeclaw-server

# Copy skill SKILL.md files for system prompt injection
COPY skills/ /app/skills/

WORKDIR /app

EXPOSE 8080

CMD ["edgeclaw-server"]
