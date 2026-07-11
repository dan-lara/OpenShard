/**
 * OpenShard Load Test — single service @ N virtual users
 *
 * Drives the public HAProxy entrypoint with a fixed Host header so traffic goes
 * through the real per-service ACL → svc_<name> backend → reverse tunnel →
 * service container. Reports latency + error rate against thesis objectives.
 *
 * Run one service @ 100 users:
 *   k6 run -e SVC_HOST=svc-a.openshard.danlara.com.br -e LABEL=svc-a load-test.js
 *
 * Environment variables:
 *   BASE_URL   target entrypoint        (default http://10.10.10.10/)
 *   SVC_HOST   Host header to route on  (default svc-a.openshard.danlara.com.br)
 *   LABEL      label for output files   (default = SVC_HOST)
 *   VUS        virtual users            (default 100)
 *   RAMP       ramp-up duration         (default 15s)
 *   DURATION   steady-state hold        (default 1m)
 *   THINK      per-user think time (s)  (default 1)   — models user pacing
 *   LAT_OBJ    latency objective (ms)   (default 300) — p95
 *   ERR_OBJ    error objective (%)      (default 0)
 */

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend } from "k6/metrics";

const BASE_URL = __ENV.BASE_URL || "http://10.10.10.10/";
const SVC_HOST = __ENV.SVC_HOST || "svc-a.openshard.danlara.com.br";
const LABEL    = __ENV.LABEL    || SVC_HOST;
const VUS      = parseInt(__ENV.VUS || "100");
const RAMP     = __ENV.RAMP || "15s";
const DURATION = __ENV.DURATION || "1m";
const THINK    = parseFloat(__ENV.THINK || "1");
const LAT_OBJ  = parseInt(__ENV.LAT_OBJ || "300");   // ms, p95
const ERR_OBJ  = parseFloat(__ENV.ERR_OBJ || "0");   // %
const OUT_DIR  = __ENV.OUT_DIR || "results";         // relative to k6 CWD

const errorRate = new Rate("openshard_errors");
const latency   = new Trend("openshard_latency", true); // ms

export const options = {
  stages: [
    { duration: RAMP,     target: VUS },  // ramp to N users
    { duration: DURATION, target: VUS },  // hold at N users  ← the reported window
    { duration: "10s",    target: 0   },  // ramp down
  ],
  thresholds: {
    "http_req_duration": [`p(95)<${LAT_OBJ}`],     // latency objective
    "http_req_failed":   [`rate<=${ERR_OBJ / 100}`], // error objective
  },
  summaryTrendStats: ["avg", "min", "med", "max", "p(90)", "p(95)", "p(99)"],
};

export default function () {
  const res = http.get(BASE_URL, {
    headers: { Host: SVC_HOST },
    timeout: "10s",
    tags: { svc: LABEL },
  });

  const ok = check(res, {
    "status 200": (r) => r.status === 200,
  });

  errorRate.add(!ok);
  latency.add(res.timings.duration);

  if (THINK > 0) sleep(THINK);
}

export function handleSummary(data) {
  const m       = data.metrics;
  const reqs    = m.http_reqs?.values?.count ?? 0;
  const rps     = (m.http_reqs?.values?.rate ?? 0);
  const avg     = (m.http_req_duration?.values?.avg ?? 0);
  const p95     = (m.http_req_duration?.values?.["p(95)"] ?? 0);
  const p99     = (m.http_req_duration?.values?.["p(99)"] ?? 0);
  const errPct  = (m.http_req_failed?.values?.rate ?? 0) * 100;

  const latPass = p95 <= LAT_OBJ;
  const errPass = errPct <= ERR_OBJ;
  const mark = (b) => (b ? "✓ OK" : "✗ FALHOU");

  const line = "─".repeat(64);
  let out = "\n" + line + "\n";
  out += `  OpenShard — Relatório de Carga  ·  ${LABEL}  ·  @ ${VUS} usuários\n`;
  out += line + "\n";
  out += `  ${"Métrica (@ " + VUS + " usuários)".padEnd(28)}${"Framework".padEnd(14)}${"Objetivo".padEnd(12)}Status\n`;
  out += `  ${"Latência (p95)".padEnd(28)}${(p95.toFixed(0) + " ms").padEnd(14)}${(LAT_OBJ + " ms").padEnd(12)}${mark(latPass)}\n`;
  out += `  ${"Taxa de erros".padEnd(28)}${(errPct.toFixed(2) + " %").padEnd(14)}${(ERR_OBJ + " %").padEnd(12)}${mark(errPass)}\n`;
  out += line + "\n";
  out += `  Latência média: ${avg.toFixed(0)} ms   ·   p99: ${p99.toFixed(0)} ms\n`;
  out += `  Requisições: ${reqs}   ·   Throughput: ${rps.toFixed(1)} req/s\n`;
  out += line + "\n";

  // Per-run JSON for the comparison report builder.
  const safe = LABEL.replace(/[^a-zA-Z0-9_.-]/g, "_");
  const row = {
    label: LABEL, vus: VUS,
    p95_ms: +p95.toFixed(0), avg_ms: +avg.toFixed(0), p99_ms: +p99.toFixed(0),
    err_pct: +errPct.toFixed(2), rps: +rps.toFixed(1), reqs,
    lat_obj_ms: LAT_OBJ, err_obj_pct: ERR_OBJ, lat_pass: latPass, err_pass: errPass,
  };

  return {
    stdout: out,
    [`${OUT_DIR}/${safe}.json`]: JSON.stringify(data, null, 2),
    [`${OUT_DIR}/${safe}.row.json`]: JSON.stringify(row, null, 2),
  };
}
