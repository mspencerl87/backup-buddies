# Multi-stage build: compile the Rust agent, then ship just the binary in a
# slim runtime image. Verified to build successfully against this exact
# dependency set (see Cargo.toml's note) as of 2026-10-01 — re-check the
# Rust image tag still supports the 2024 edition (stable since Rust 1.85)
# before bumping.
FROM rust:1.90-bookworm AS builder

# aws-lc-rs (pulled in by reqwest's TLS stack) builds a C library via cmake.
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/backup-buddies-client /usr/local/bin/backup-buddies-client

# Persistent state (the Iroh identity key + anything received from a buddy)
# lives here — the customer's docker-compose.yml mounts a named volume or
# host directory at this path.
VOLUME ["/data"]
ENV DATA_DIR=/data

ENTRYPOINT ["/usr/local/bin/backup-buddies-client"]
