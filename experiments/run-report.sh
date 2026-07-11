#!/usr/bin/env bash
# OpenShard load-test report builder.
# Runs the @N-users load test against each target, then assembles a Markdown
# comparison report (latency + error rate vs. objective).
#
# Usage:  ./run-report.sh                  # default targets, 100 VUs, 1m hold
#         VUS=100 DURATION=1m ./run-report.sh
set -euo pipefail
cd "$(dirname "$0")"

BASE_URL=${BASE_URL:-http://10.10.10.10/}
VUS=${VUS:-100}
DURATION=${DURATION:-1m}
RAMP=${RAMP:-15s}
THINK=${THINK:-1}
LAT_OBJ=${LAT_OBJ:-300}
ERR_OBJ=${ERR_OBJ:-0}
RESULTS=results

# Targets to benchmark:  "label=Host header"
TARGETS=(
  "svc-a (sem limite)=svc-a.openshard.danlara.com.br"
  "svc-pc (0.5 CPU / 256MB)=svc-pc.openshard.danlara.com.br"
)

mkdir -p "$RESULTS"
rm -f "$RESULTS"/*.row.json 2>/dev/null || true

for entry in "${TARGETS[@]}"; do
  label="${entry%%=*}"
  host="${entry#*=}"
  echo ">> Testando: $label  ($host)  @ ${VUS} usuários"
  k6 run \
    -e BASE_URL="$BASE_URL" \
    -e SVC_HOST="$host" \
    -e LABEL="$label" \
    -e VUS="$VUS" -e RAMP="$RAMP" -e DURATION="$DURATION" -e THINK="$THINK" \
    -e LAT_OBJ="$LAT_OBJ" -e ERR_OBJ="$ERR_OBJ" \
    load-test.js
done

# Assemble the comparison report from the per-run rows.
python3 - "$RESULTS" "$VUS" <<'PY'
import json, sys, glob, os
results, vus = sys.argv[1], sys.argv[2]
rows = [json.load(open(f)) for f in sorted(glob.glob(os.path.join(results, "*.row.json")))]
md = []
md.append(f"# OpenShard — Relatório de Carga (@ {vus} usuários)\n")
md.append("| Serviço | Latência p95 | Objetivo | Taxa de erros | Objetivo | Status |")
md.append("|---|---|---|---|---|---|")
for r in rows:
    status = "✅ OK" if (r["lat_pass"] and r["err_pass"]) else "❌ FALHOU"
    md.append(f"| {r['label']} | {r['p95_ms']} ms | {r['lat_obj_ms']} ms | "
              f"{r['err_pct']:.2f} % | {r['err_obj_pct']} % | {status} |")
md.append("")
md.append("| Serviço | Latência média | p99 | Throughput | Requisições |")
md.append("|---|---|---|---|---|")
for r in rows:
    md.append(f"| {r['label']} | {r['avg_ms']} ms | {r['p99_ms']} ms | "
              f"{r['rps']:.1f} req/s | {r['reqs']} |")
report = "\n".join(md) + "\n"
open(os.path.join(results, "report.md"), "w").write(report)
print("\n" + report)
print(f"Relatório salvo em: {os.path.join(results, 'report.md')}")
PY
