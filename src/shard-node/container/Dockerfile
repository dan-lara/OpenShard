FROM python:3.12-alpine

RUN apk add --no-cache nginx

# Test service
COPY test-service/ /usr/share/nginx/html/

RUN apk add --no-cache nginx curl build-base && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

# Nginx config uses a placeholder replaced at runtime by entrypoint
RUN printf 'server {\n    listen SERVICE_PORT_PLACEHOLDER;\n    root /usr/share/nginx/html;\n    index index.html;\n    location /health {\n        return 200 "ok";\n        add_header Content-Type text/plain;\n    }\n}\n' > /etc/nginx/http.d/default.conf

COPY client.rs proto.rs ./
COPY Cargo.toml ./
RUN cargo build --release

WORKDIR /app
COPY agent.py .
COPY entrypoint.sh .
RUN chmod +x entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/app/entrypoint.sh"]
