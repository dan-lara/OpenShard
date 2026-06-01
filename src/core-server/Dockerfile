# ─────────────────────────────────────────────────────────────────────────────
# Stage 1 — Standard Rust builder (Dynamic GNU build)
# ─────────────────────────────────────────────────────────────────────────────
FROM rust:1.86-bookworm AS builder

WORKDIR /build

# ── dependency cache layer ────────────────────────────────────────────────────
COPY haproxy-manager/Cargo.toml haproxy-manager/Cargo.toml
COPY registrar/Cargo.toml       registrar/Cargo.toml
COPY tunnel/Cargo.toml          tunnel/Cargo.toml

RUN cargo fetch --manifest-path haproxy-manager/Cargo.toml 2>/dev/null || true
RUN cargo fetch --manifest-path registrar/Cargo.toml       2>/dev/null || true
RUN cargo fetch --manifest-path tunnel/Cargo.toml          2>/dev/null || true

# ── copy real source ──────────────────────────────────────────────────────────
COPY haproxy-manager/ haproxy-manager/
COPY registrar/       registrar/
COPY tunnel/          tunnel/

# Build dynamically linked binaries (native architecture)
RUN cargo build --release --manifest-path registrar/Cargo.toml
RUN cargo build --release --manifest-path tunnel/Cargo.toml


# ─────────────────────────────────────────────────────────────────────────────
# Stage 2 — registrar + HAProxy runtime image
# ─────────────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS registrar

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        haproxy \
        supervisor \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /run/haproxy /opt/openshard/data /etc/haproxy /etc/supervisor/conf.d

COPY haproxy-manager/haproxy.cfg /etc/haproxy/haproxy.cfg
COPY docker/supervisord.conf /etc/supervisor/supervisord.conf

# Assuming a Cargo workspace, artifacts map to the root target directory.
COPY --from=builder /build/registrar/target/release/registrar /opt/openshard/registrar
RUN chmod +x /opt/openshard/registrar

ENV RUST_LOG=info \
    HAPROXY_CFG=/etc/haproxy/haproxy.cfg \
    HAPROXY_SOCKET=/run/haproxy/admin.sock \
    OPENSHARD_DB=/opt/openshard/data/openshard.db

EXPOSE 80 3000 8405

HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -sf http://localhost:3000/volunteers || exit 1

CMD ["/usr/bin/supervisord", "-c", "/etc/supervisor/supervisord.conf"]


# ─────────────────────────────────────────────────────────────────────────────
# Stage 3 — tunnel-server runtime image
# ─────────────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS tunnel

# Required libraries (libc, libgcc_s, libm) are provided by bookworm-slim.
COPY --from=builder /build/tunnel/target/release/server /tunnel-server

CMD ["/tunnel-server"]