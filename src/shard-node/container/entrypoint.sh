#!/bin/sh
set -e

PORT=${SERVICE_PORT:-8080}
REGISTRAR=${REGISTRAR_URL:-http://localhost:3000}

echo "================================================"
echo " OpenShard Volunteer Node"
echo " Registrar : $REGISTRAR"
echo " Service   : :$PORT"
echo "================================================"

# Replace placeholder with actual port at runtime
sed -i "s/SERVICE_PORT_PLACEHOLDER/$PORT/g" /etc/nginx/http.d/default.conf

echo "[entrypoint] Starting nginx on :$PORT..."
nginx -g "daemon off;" &
NGINX_PID=$!

sleep 1

echo "[entrypoint] Starting tunnel..."
./client
sleep 1

echo "[entrypoint] Starting volunteer agent..."
python3 /app/agent.py &
AGENT_PID=$!

while kill -0 $NGINX_PID 2>/dev/null && kill -0 $AGENT_PID 2>/dev/null; do
    sleep 2
done

echo "[entrypoint] A process exited - shutting down"
kill $NGINX_PID $AGENT_PID 2>/dev/null
exit 1