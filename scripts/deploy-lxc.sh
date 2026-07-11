#!/bin/bash
# scripts/deploy-lxc.sh
# Runs INSIDE the LXC — replaces the registrar binary and updates HAProxy config
set -e

DEPLOY_DIR="/tmp/openshard-deploy"
INSTALL_DIR="/opt/openshard"
BINARY="registrar"
SERVICE="registrar"
HAPROXY_CFG="/etc/haproxy/haproxy.cfg"
HAPROXY_CFG_BACKUP="/etc/haproxy/haproxy.cfg.bak"

echo "==> Starting deploy inside LXC..."

# Create install dirs
mkdir -p "$INSTALL_DIR"
mkdir -p "$INSTALL_DIR/data"

# ── Registrar binary ──────────────────────────────────────────────────────────
echo "==> Stopping registrar..."
systemctl stop "$SERVICE" 2>/dev/null || true

echo "==> Installing registrar binary..."
cp "$DEPLOY_DIR/$BINARY" "$INSTALL_DIR/$BINARY"
chmod +x "$INSTALL_DIR/$BINARY"

# ── HAProxy config ────────────────────────────────────────────────────────────
echo "==> Validating new HAProxy config..."
haproxy -c -f "$DEPLOY_DIR/haproxy.cfg"

if [ $? -ne 0 ]; then
    echo "ERROR: HAProxy config validation failed — aborting, nothing changed."
    exit 1
fi

echo "==> Backing up current HAProxy config..."
cp "$HAPROXY_CFG" "$HAPROXY_CFG_BACKUP"

echo "==> Applying new HAProxy config..."
cp "$DEPLOY_DIR/haproxy.cfg" "$HAPROXY_CFG"

echo "==> Reloading HAProxy gracefully..."
systemctl reload haproxy

if [ $? -ne 0 ]; then
    echo "ERROR: HAProxy reload failed — restoring backup..."
    cp "$HAPROXY_CFG_BACKUP" "$HAPROXY_CFG"
    systemctl reload haproxy
    exit 1
fi

echo "==> HAProxy reloaded successfully."

# ── Registrar service ─────────────────────────────────────────────────────────
# Install systemd service if not present
if [ ! -f "/etc/systemd/system/$SERVICE.service" ]; then
    echo "==> Installing systemd service..."
    cat > "/etc/systemd/system/$SERVICE.service" << EOF
[Unit]
Description=OpenShard Registrar
After=network.target haproxy.service

[Service]
ExecStart=/opt/openshard/registrar
WorkingDirectory=/opt/openshard
Restart=always
RestartSec=3
Environment=RUST_LOG=info
Environment=HAPROXY_CFG=/etc/haproxy/haproxy.cfg
Environment=HAPROXY_SOCKET=/run/haproxy/admin.sock
Environment=HAPROXY_PID=/run/haproxy/haproxy.pid
Environment=OPENSHARD_DB=/opt/openshard/data/openshard.db

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable "$SERVICE"
fi

echo "==> Restarting registrar..."
systemctl restart "$SERVICE"
sleep 2
systemctl status "$SERVICE" --no-pager

echo ""
echo "==> Deploy finished successfully."
echo "    HAProxy:   $(systemctl is-active haproxy)"
echo "    Registrar: $(systemctl is-active registrar)"