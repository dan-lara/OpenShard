# ─────────────────────────────────────────────────────────────────────────────
# Stage 1 — Rust builder
# Builds both the `registrar` binary and the standalone `tunnel-server` binary.
# ─────────────────────────────────────────────────────────────────────────────
FROM rust:1.86-bookworm AS builder

WORKDIR /build

# ── dependency cache layer ────────────────────────────────────────────────────
# Copy manifests only — cargo fetch does not need source files.
COPY haproxy-manager/Cargo.toml haproxy-manager/Cargo.toml
COPY registrar/Cargo.toml       registrar/Cargo.toml

RUN cargo fetch --manifest-path haproxy-manager/Cargo.toml 2>/dev/null || true
RUN cargo fetch --manifest-path registrar/Cargo.toml       2>/dev/null || true

# ── copy real source ──────────────────────────────────────────────────────────
COPY haproxy-manager/ haproxy-manager/
COPY registrar/       registrar/

# Build registrar (release)
RUN cargo build --release --manifest-path registrar/Cargo.toml

# ── tunnel server (server.rs is a self-contained single-file binary) ──────────
# server.rs uses `mod proto;` — both files must sit next to each other.
COPY tunnel/server.rs tunnel/server.rs
COPY tunnel/proto.rs  tunnel/proto.rs

RUN rustc --edition 2021 -O -o /build/target/release/tunnel-server \
      tunnel/server.rs \
      2>&1


# ─────────────────────────────────────────────────────────────────────────────
# Stage 2 — registrar + HAProxy runtime image
#
# Runs:
#   • haproxy   (managed by supervisord)
#   • registrar (managed by supervisord)
# ─────────────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS registrar

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        haproxy \
        supervisor \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p \
        /run/haproxy \
        /opt/openshard/data \
        /etc/haproxy

# haproxy.cfg lives inside haproxy-manager/
COPY haproxy-manager/haproxy.cfg /etc/haproxy/haproxy.cfg

COPY --from=builder /build/target/release/registrar /opt/openshard/registrar
RUN chmod +x /opt/openshard/registrar

RUN mkdir -p /etc/supervisor/conf.d
COPY docker/supervisord.conf /etc/supervisor/supervisord.conf

ENV RUST_LOG=info \
    HAPROXY_CFG=/etc/haproxy/haproxy.cfg \
    HAPROXY_SOCKET=/run/haproxy/admin.sock \
    OPENSHARD_DB=/opt/openshard/data/openshard.db

EXPOSE 80 3000 8405

HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -sf http://localhost:3000/volunteers || exit 1

CMD ["/usr/bin/supervisord", "-c", "/etc/supervisor/supervisord.conf"]


# ─────────────────────────────────────────────────────────────────────────────
# Stage 3 — tunnel-server image
# ─────────────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS tunnel

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/tunnel-server /usr/local/bin/tunnel-server
RUN chmod +x /usr/local/bin/tunnel-server

CMD ["/usr/local/bin/tunnel-server"]