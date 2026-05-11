FROM python:3.12-alpine

RUN apk add --no-cache nginx

# Test service
COPY test-service/ /usr/share/nginx/html/

# Nginx config uses a placeholder replaced at runtime by entrypoint
RUN printf 'server {\n    listen SERVICE_PORT_PLACEHOLDER;\n    root /usr/share/nginx/html;\n    index index.html;\n    location /health {\n        return 200 "ok";\n        add_header Content-Type text/plain;\n    }\n}\n' > /etc/nginx/http.d/default.conf

WORKDIR /app
COPY agent.py .
COPY entrypoint.sh .
RUN chmod +x entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/app/entrypoint.sh"]
