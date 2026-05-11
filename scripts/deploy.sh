#!/bin/bash
# scripts/deploy.sh
# Sends the build package to Proxmox and deploys into the HAProxy LXC
set -e

PROXMOX_HOST="root@100.81.196.38"
LXC_ID="102"
PACKAGE="/tmp/openshard-deploy.tar.gz"

if [ ! -f "$PACKAGE" ]; then
    echo "==> Package not found. Running build first..."
    bash scripts/build.sh
fi

echo "==> Sending package to Proxmox..."
scp "$PACKAGE" "$PROXMOX_HOST:/tmp/openshard-deploy.tar.gz"

echo "==> Pushing package into LXC $LXC_ID..."
ssh "$PROXMOX_HOST" "pct push $LXC_ID /tmp/openshard-deploy.tar.gz /tmp/openshard-deploy.tar.gz"

echo "==> Running deploy script inside LXC..."
ssh "$PROXMOX_HOST" "pct exec $LXC_ID -- bash -c '
    cd /tmp &&
    tar -xzf openshard-deploy.tar.gz &&
    bash /tmp/openshard-deploy/deploy-lxc.sh
'"

echo "==> Deploy complete."