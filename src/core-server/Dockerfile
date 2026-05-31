# ─────────────────────────────────────────────────────────────────────────────
# Stage 1 — Rust builder (fully static musl build)
# ─────────────────────────────────────────────────────────────────────────────
FROM rust:1.86-bookworm AS builder

RUN rustup target add x86_64-unknown-linux-musl && \
    apt-get update && \
    apt-get install -y --no-install-recommends \
        musl-tools \
    && rm -rf /var/lib/apt/lists/*

# Force ALL compilation (including build scripts, which run on the host target)
# to use musl-gcc so the broken GNU libutil.a is never touched.
ENV CC=musl-gcc \
    CC_x86_64_unknown_linux_musl=musl-gcc \
    CC_x86_64_unknown_linux_gnu=musl-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=musl-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc

WORKDIR /build

# ── dependency cache layer ────────────────────────────────────────────────────
COPY haproxy-manager/Cargo.toml haproxy-manager/Cargo.toml
COPY registrar/Cargo.toml       registrar/Cargo.toml

RUN cargo fetch --manifest-path haproxy-manager/Cargo.toml 2>/dev/null || true
RUN cargo fetch --manifest-path registrar/Cargo.toml       2>/dev/null || true

# ── copy real source ──────────────────────────────────────────────────────────
COPY haproxy-manager/ haproxy-manager/
COPY registrar/       registrar/

# Build registrar as a fully static binary
RUN cargo build --release \
      --manifest-path registrar/Cargo.toml \
      --target x86_64-unknown-linux-musl

# ── tunnel server ─────────────────────────────────────────────────────────────
COPY tunnel/server.rs tunnel/server.rs
COPY tunnel/proto.rs  tunnel/proto.rs

RUN rustc --edition 2021 -O \
      --target x86_64-unknown-linux-musl \
      -o /build/tunnel-server \
      tunnel/server.rs


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

RUN mkdir -p \
        /run/haproxy \
        /opt/openshard/data \
        /etc/haproxy

COPY haproxy-manager/haproxy.cfg /etc/haproxy/haproxy.cfg

COPY --from=builder /build/registrar/target/x86_64-unknown-linux-musl/release/registrar \
                    /opt/openshard/registrar
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
FROM scratch AS tunnel

COPY --from=builder /build/tunnel-server /tunnel-server

CMD ["/tunnel-server"]