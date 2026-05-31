# ─── Stage 1: build ────────────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

# System deps for rusqlite bundled feature (needs cc + libsqlite3 headers)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy workspace manifests and source
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Build release binary
RUN cargo build --release -p omrp-runtime

# ─── Stage 2: runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Non-root user for the server process
RUN useradd -r -s /bin/false -m -d /app omrp
USER omrp
WORKDIR /app

# Copy compiled binary
COPY --from=builder /build/target/release/omrp /app/omrp

# Data directory (DB and config live here — mount a volume for persistence)
RUN mkdir -p /app/db

EXPOSE 18800

# Default: start the web server on all interfaces
CMD ["/app/omrp", "serve", "--host", "0.0.0.0", "--port", "18800"]
