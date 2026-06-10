# ─────────────────────────────────────────────────────────────────────────────
# Stage 1 — Rust Builder (Alpine + musl)
# ─────────────────────────────────────────────────────────────────────────────
FROM rust:alpine AS builder

# Required for compiling C dependencies in Alpine
RUN apk add --no-cache musl-dev

WORKDIR /build

# Copy the Rust client source files
COPY Cargo.toml ./
COPY client.rs proto.rs ./

# Compile the static musl binary. 
# (Assuming the package name in Cargo.toml is "client")
RUN cargo build --release


# ─────────────────────────────────────────────────────────────────────────────
# Stage 2 — Python + Nginx Runtime
# ─────────────────────────────────────────────────────────────────────────────
FROM python:3.12-alpine

# Install Nginx and Docker CLI (agent.py manages service containers via the host socket)
RUN apk add --no-cache nginx docker-cli

# Nginx config uses a placeholder replaced at runtime by entrypoint
RUN printf 'server {\n    listen SERVICE_PORT_PLACEHOLDER;\n    root /usr/share/nginx/html;\n    index index.html;\n    location /health {\n        return 200 "ok";\n        add_header Content-Type text/plain;\n    }\n}\n' > /etc/nginx/http.d/default.conf

# Setup test service static files
COPY test-service/ /usr/share/nginx/html/

WORKDIR /app

# Copy Python agent and startup script
COPY agent.py entrypoint.sh ./
RUN chmod +x entrypoint.sh

# Copy the compiled Rust binary from the builder stage
COPY --from=builder /build/target/release/client /app/client

EXPOSE 8080

# The entrypoint script is responsible for starting Nginx, the Python agent, 
# and the Rust client.
ENTRYPOINT ["/app/entrypoint.sh"]