#!/bin/bash
# scripts/build.sh
# Compiles registrar in release mode and packages it for deployment
set -e

BINARY_NAME="registrar"
PACKAGE_DIR="/tmp/openshard-deploy"
HAPROXY_CFG="src/core-server/haproxy-manager/haproxy.cfg"

echo "==> Building registrar (release)..."
# cargo build --release -p registrar
cargo build --release -p registrar --target x86_64-unknown-linux-musl

echo "==> Packaging..."
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR"

# Binary
cp target/x86_64-unknown-linux-musl/release/$BINARY_NAME "$PACKAGE_DIR/"

# HAProxy config
cp "$HAPROXY_CFG" "$PACKAGE_DIR/haproxy.cfg"

# Deploy script (will run inside LXC)
cp scripts/deploy-lxc.sh "$PACKAGE_DIR/deploy-lxc.sh"
chmod +x "$PACKAGE_DIR/deploy-lxc.sh"

# Create tarball
tar -czf /tmp/openshard-deploy.tar.gz -C /tmp openshard-deploy

echo "==> Built successfully:"
ls -lh /tmp/openshard-deploy.tar.gz
echo ""
echo "==> Next: run 'make deploy' to send to server"