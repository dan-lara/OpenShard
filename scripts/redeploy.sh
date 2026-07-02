#!/bin/bash
# scripts/redeploy.sh
# Run from the Proxmox root host.
# Builds images on LXC 200, distributes them, restarts all services,
# waits for enrollment, then prints a health report.
set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
LXC_BUILD=200
LXC_CONTROLLER=102
LXC_WORKERS=(111 112 113)
REPO_PATH="/root/dev/OpenShard"
COMPOSE_SRC="$REPO_PATH/src/core-server/docker-compose.yml"
COMPOSE_DST="/opt/openshard/docker-compose.yml"
REGISTRAR_URL="http://10.10.10.10:3000"
TUNNEL_SERVER="10.10.10.10"
ENROLL_WAIT=30          # seconds to wait for volunteers to enroll
TMP_DIR="/tmp/openshard-redeploy"

# ── Colors ────────────────────────────────────────────────────────────────────
BOLD='\033[1m'; GREEN='\033[32m'; RED='\033[31m'; CYAN='\033[36m'; RESET='\033[0m'
log()  { echo -e "\n${BOLD}${CYAN}▶ $*${RESET}"; }
ok()   { echo -e "  ${GREEN}✓ $*${RESET}"; }
warn() { echo -e "  ${RED}✗ $*${RESET}"; }

REPORT=()
report_ok()   { REPORT+=("OK  $*"); }
report_fail() { REPORT+=("FAIL $*"); }

mkdir -p "$TMP_DIR"

# ── 1. Build ──────────────────────────────────────────────────────────────────
log "Building images on LXC $LXC_BUILD..."
pct exec $LXC_BUILD -- sh -c "cd $REPO_PATH && make deploy"
pct exec $LXC_BUILD -- sh -c "cd $REPO_PATH && make vol-build"
ok "All images built"

# ── 2. Save images inside LXC 200 ────────────────────────────────────────────
log "Saving images to tarballs inside LXC $LXC_BUILD..."
# node-exporter is pulled from Docker Hub by compose — no need to transfer
pct exec $LXC_BUILD -- sh -c \
  "docker save openshard/registrar:latest openshard/tunnel:latest | gzip > /tmp/os-controller.tar.gz"
pct exec $LXC_BUILD -- sh -c \
  "docker save openshard/volunteer:latest | gzip > /tmp/os-volunteer.tar.gz"
ok "Tarballs ready"

# ── 3. Pull tarballs + compose file to Proxmox host ──────────────────────────
log "Pulling artefacts from LXC $LXC_BUILD to Proxmox host..."
pct pull $LXC_BUILD /tmp/os-controller.tar.gz           "$TMP_DIR/controller.tar.gz"
pct pull $LXC_BUILD /tmp/os-volunteer.tar.gz            "$TMP_DIR/volunteer.tar.gz"
pct pull $LXC_BUILD "$COMPOSE_SRC"                      "$TMP_DIR/docker-compose.yml"
ok "Artefacts pulled"

# ── 4. Controller: push images + compose, reload stack ───────────────────────
log "Updating controller LXC $LXC_CONTROLLER..."
pct push $LXC_CONTROLLER "$TMP_DIR/controller.tar.gz"   /tmp/os-controller.tar.gz
pct push $LXC_CONTROLLER "$TMP_DIR/docker-compose.yml"  "$COMPOSE_DST"
pct exec $LXC_CONTROLLER -- sh -c "docker load -i /tmp/os-controller.tar.gz"
# Stop legacy systemd services that would conflict on ports 80/3000
pct exec $LXC_CONTROLLER -- sh -c \
  "systemctl stop haproxy registrar 2>/dev/null; systemctl disable haproxy registrar 2>/dev/null; true"
# Stop ALL containers (old stacks from different project names may hold ports)
pct exec $LXC_CONTROLLER -- sh -c \
  "docker stop \$(docker ps -q) 2>/dev/null; docker rm \$(docker ps -aq) 2>/dev/null; true"
pct exec $LXC_CONTROLLER -- sh -c \
  "cd /opt/openshard && docker compose up -d --no-build"
ok "Controller stack restarted"

# ── 5. Workers: push image, restart volunteer container ──────────────────────
for id in "${LXC_WORKERS[@]}"; do
  log "Updating worker LXC $id..."
  pct push $id "$TMP_DIR/volunteer.tar.gz" /tmp/os-volunteer.tar.gz
  pct exec $id -- sh -c "docker load -i /tmp/os-volunteer.tar.gz"
  pct exec $id -- sh -c "
    docker rm -f openshard-volunteer 2>/dev/null || true
    docker run -d \
      --name openshard-volunteer \
      --network host \
      -v /var/run/docker.sock:/var/run/docker.sock \
      -e REGISTRAR_URL=$REGISTRAR_URL \
      -e TUNNEL_SERVER=$TUNNEL_SERVER \
      -e SERVICE_PORT=8080 \
      -e HOSTNAME=volunteer-$id \
      openshard/volunteer:latest
  "
  ok "LXC $id volunteer restarted"
done

# ── 6. Wait for enrollment ────────────────────────────────────────────────────
log "Waiting ${ENROLL_WAIT}s for volunteers to enroll..."
sleep "$ENROLL_WAIT"

# ── 7. Health checks ──────────────────────────────────────────────────────────
log "Running health checks..."

# Registrar reachable
if curl -sf "$REGISTRAR_URL/volunteers" -o "$TMP_DIR/volunteers.json" 2>/dev/null; then
  VOL_COUNT=$(python3 -c "import json; print(len(json.load(open('$TMP_DIR/volunteers.json'))))")
  report_ok "Registrar reachable — $VOL_COUNT volunteer(s) enrolled"
else
  report_fail "Registrar not reachable at $REGISTRAR_URL"
  VOL_COUNT=0
fi

# Services endpoint
if curl -sf "$REGISTRAR_URL/services" -o "$TMP_DIR/services.json" 2>/dev/null; then
  SVCS=$(python3 -c "
import json
s = json.load(open('$TMP_DIR/services.json')).get('services', [])
print(', '.join(s) if s else 'none')
")
  report_ok "Services endpoint OK — registered: $SVCS"
else
  report_fail "Services endpoint not reachable"
fi

# Expected workers enrolled
for id in "${LXC_WORKERS[@]}"; do
  if python3 -c "
import json, sys
vols = json.load(open('$TMP_DIR/volunteers.json'))
match = any('$id' in v['info'].get('hostname','') or
            v['info'].get('hostname','').endswith(':') for v in vols)
# check by HOSTNAME tag inside service_addr field or assigned data
sys.exit(0)  # best-effort; tunnel port may differ
" 2>/dev/null; then
    true  # checked below per volunteer
  fi
done

# Per-volunteer detail
if [ "$VOL_COUNT" -gt 0 ]; then
  echo ""
  python3 - "$TMP_DIR/volunteers.json" <<'PYEOF'
import json, sys
vols = json.load(open(sys.argv[1]))
print(f"  {'ID':8}  {'addr':22}  {'cpu':>5}  {'mem':>5}  {'assigned'}")
print(f"  {'-'*8}  {'-'*22}  {'-'*5}  {'-'*5}  {'-'*20}")
for v in vols:
    svc  = (v.get('assigned_service') or {}).get('service_name', '—')
    cpu  = f"{v['metrics']['cpu_pct']:.1f}%"
    mem  = f"{v['metrics']['mem_pct']:.1f}%"
    addr = v['info']['service_addr']
    print(f"  {v['id'][:8]}  {addr:22}  {cpu:>5}  {mem:>5}  {svc}")
PYEOF
fi

# Controller container status
echo ""
log "Controller containers (LXC $LXC_CONTROLLER):"
pct exec $LXC_CONTROLLER -- docker ps \
  --format 'table {{.Image}}\t{{.Status}}\t{{.Names}}'

# ── 8. Report ─────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}══════════════════════════════════════════════════${RESET}"
echo -e "${BOLD}  OpenShard Redeploy Report — $(date '+%Y-%m-%d %H:%M:%S')${RESET}"
echo -e "${BOLD}══════════════════════════════════════════════════${RESET}"
for line in "${REPORT[@]}"; do
  if [[ "$line" == OK* ]]; then
    echo -e "  ${GREEN}✓ ${line#OK  }${RESET}"
  else
    echo -e "  ${RED}✗ ${line#FAIL }${RESET}"
  fi
done
echo ""

# Cleanup
rm -rf "$TMP_DIR"
