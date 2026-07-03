#!/bin/bash
# update_hosts.sh
# Rebuilds and restarts volunteer containers on nodes 111 and 112
set -e

NODES="111 112"
PACKAGE="/tmp/volunteer-src.tar.gz"

echo "==> Pushing package to nodes..."
for CT in $NODES; do
  pct push $CT $PACKAGE /tmp/volunteer-src.tar.gz
done

echo "==> Building and restarting on each node..."
for CT in $NODES; do
  echo "--- Node $CT ---"
  pct exec $CT -- bash -c "
    mkdir -p /opt/volunteer &&
    tar -xzf /tmp/volunteer-src.tar.gz -C /opt/volunteer &&
    docker build -t openshard/volunteer:new /opt/volunteer &&
    docker stop volunteer 2>/dev/null || true &&
    docker rm volunteer 2>/dev/null || true &&
    docker rmi openshard/volunteer:latest 2>/dev/null || true &&
    docker tag openshard/volunteer:new openshard/volunteer:latest &&
    docker rmi openshard/volunteer:new &&
    docker run -d \
      --name volunteer \
      --restart unless-stopped \
      --env-file /opt/volunteer/.env \
      -p 8080:8080 \
      openshard/volunteer:latest
  "
done

echo "==> Verifying images..."
for CT in $NODES; do
  echo "--- Node $CT ---"
  pct exec $CT -- docker ps --filter name=volunteer --format "table {{.Names}}\t{{.Status}}\t{{.Image}}"
done

echo ""
echo "==> Done. Waiting 20s for enrollment..."
sleep 20

echo "==> Checking volunteers on registrar..."
pct exec 102 -- curl -s http://localhost:3000/volunteers | python3 -m json.tool