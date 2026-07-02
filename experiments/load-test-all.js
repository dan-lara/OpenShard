/**
 * OpenShard Load Test — ALL services in one run, @ N virtual users.
 *
 * Each iteration round-robins across every service (Host header → svc_<name>
 * backend → tunnel → container), so one sustained run produces per-service
 * latency + error rate. Intended to be run from a remote machine so the
 * measured latency includes real network RTT.
 *
 * Run 7 minutes @ 100 users:
 *   k6 run -e BASE_URL=http://10.10.10.10/ -e VUS=100 -e DURATION=7m load-test-all.js
 *
 * Environment variables:
 *   BASE_URL   HAProxy entrypoint reachable from here (default http://10.10.10.10/)
 *   VUS        virtual users            (default 100)
 *   RAMP       ramp-up duration         (default 30s)
 *   DURATION   steady-state hold        (default 7m)   ← the reported window
 *   THINK      per-user think time (s)  (default 1)
 *   LAT_OBJ    latency objective (ms)   (default 300)  p95
 *   ERR_OBJ    error objective (%)      (default 0)
 *   OUT_DIR    output dir               (default results)
 */

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend } from "k6/metrics";

const BASE_URL = __ENV.BASE_URL || "http://10.10.10.10/";
const VUS      = parseInt(__ENV.VUS || "100");
const RAMP     = __ENV.RAMP || "30s";
const DURATION = __ENV.DURATION || "7m";
const THINK    = parseFloat(__ENV.THINK || "1");
const LAT_OBJ  = parseInt(__ENV.LAT_OBJ || "300");
const ERR_OBJ  = parseFloat(__ENV.ERR_OBJ || "0");
const OUT_DIR  = __ENV.OUT_DIR || "results";

// Every service behind the controller. Edit if you add/remove services.
const SERVICES = [
  { label: "svc-a (sem limite)",        host: "svc-a.openshard.danlara.com.br" },
  { label: "svc-b (sem limite)",        host: "svc-b.openshard.danlara.com.br" },
  { label: "svc-index (sem limite)",    host: "openshard.danlara.com.br" },
  { label: "svc-pc (0.5 CPU / 256MB)",  host: "svc-pc.openshard.danlara.com.br" },
];

// One latency Trend + error Rate per service.
const M = {};
for (const s of SERVICES) {
  const k = s.label.replace(/[^a-zA-Z0-9_]/g, "_");
  M[s.label] = { key: k, lat: new Trend(`lat_${k}`, true), err: new Rate(`err_${k}`) };
}

export const options = {
  stages: [
    { duration: RAMP,     target: VUS },
    { duration: DURATION, target: VUS },
    { duration: "10s",    target: 0   },
  ],
  thresholds: {
    "http_req_duration": [`p(95)<${LAT_OBJ}`],
    "http_req_failed":   [`rate<=${ERR_OBJ / 100}`],
  },
  summaryTrendStats: ["avg", "min", "med", "max", "p(90)", "p(95)", "p(99)"],
};

export default function () {
  const s = SERVICES[(__VU + __ITER) % SERVICES.length];
  const res = http.get(BASE_URL, {
    headers: { Host: s.host },
    timeout: "10s",
    tags: { svc: s.label },
  });
  const ok = check(res, { "status 200": (r) => r.status === 200 }, { svc: s.label });
  M[s.label].err.add(!ok);
  M[s.label].lat.add(res.timings.duration);
  if (THINK > 0) sleep(THINK);
}

export function handleSummary(data) {
  const line = "─".repeat(78);
  const mark = (b) => (b ? "✓ OK" : "✗ FALHOU");
  const rows = [];

  let out = "\n" + line + "\n";
  out += `  OpenShard — Relatório de Carga (todos os serviços) · @ ${VUS} usuários · ${DURATION}\n`;
  out += line + "\n";
  out += `  ${"Serviço".padEnd(28)}${"p95".padEnd(9)}${"média".padEnd(9)}${"p99".padEnd(9)}${"erros".padEnd(9)}Status\n`;

  for (const s of SERVICES) {
    const lat = data.metrics[`lat_${M[s.label].key}`]?.values ?? {};
    const err = data.metrics[`err_${M[s.label].key}`]?.values ?? {};
    const p95 = lat["p(95)"] ?? 0, avg = lat.avg ?? 0, p99 = lat["p(99)"] ?? 0;
    const errPct = (err.rate ?? 0) * 100;
    const latPass = p95 <= LAT_OBJ, errPass = errPct <= ERR_OBJ;
    out += `  ${s.label.padEnd(28)}${(p95.toFixed(0)+"ms").padEnd(9)}${(avg.toFixed(0)+"ms").padEnd(9)}${(p99.toFixed(0)+"ms").padEnd(9)}${(errPct.toFixed(2)+"%").padEnd(9)}${mark(latPass && errPass)}\n`;
    rows.push({ label: s.label, p95_ms: +p95.toFixed(0), avg_ms: +avg.toFixed(0),
                p99_ms: +p99.toFixed(0), err_pct: +errPct.toFixed(2),
                lat_obj_ms: LAT_OBJ, err_obj_pct: ERR_OBJ, lat_pass: latPass, err_pass: errPass });
  }
  out += line + `\n  Objetivo: p95 < ${LAT_OBJ} ms · erros = ${ERR_OBJ} %\n` + line + "\n";

  // Markdown report
  let md = `# OpenShard — Relatório de Carga (@ ${VUS} usuários, ${DURATION})\n\n`;
  md += "| Serviço | Latência p95 | Objetivo | Taxa de erros | Objetivo | Status |\n|---|---|---|---|---|---|\n";
  for (const r of rows) {
    const st = (r.lat_pass && r.err_pass) ? "✅ OK" : "❌ FALHOU";
    md += `| ${r.label} | ${r.p95_ms} ms | ${r.lat_obj_ms} ms | ${r.err_pct.toFixed(2)} % | ${r.err_obj_pct} % | ${st} |\n`;
  }
  md += "\n| Serviço | Latência média | p99 |\n|---|---|---|\n";
  for (const r of rows) md += `| ${r.label} | ${r.avg_ms} ms | ${r.p99_ms} ms |\n`;

  return {
    stdout: out,
    [`${OUT_DIR}/report-all.md`]: md,
    [`${OUT_DIR}/summary-all.json`]: JSON.stringify(data, null, 2),
  };
}
