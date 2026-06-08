/**
 * OpenShard Load Test
 *
 * Targets the public reverse-proxy endpoint and measures latency/throughput
 * under a ramp-up scenario suitable for thesis benchmarking.
 *
 * Run (with Prometheus remote write for live dashboard):
 *   k6 run --out experimental-prometheus-rw load-test.js
 *
 * Run (HTML report only, no Prometheus):
 *   K6_WEB_DASHBOARD=true K6_WEB_DASHBOARD_EXPORT=report.html k6 run load-test.js
 *
 * Environment variables:
 *   TARGET_URL   — override the default target (default: https://openshrd.danlara.com.br)
 *   MAX_VUS      — peak virtual users (default: 50)
 */

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend } from "k6/metrics";

const TARGET_URL = __ENV.TARGET_URL || "https://openshrd.danlara.com.br";
const MAX_VUS    = parseInt(__ENV.MAX_VUS || "50");

// ── Custom metrics ────────────────────────────────────────────────────────────
const errorRate  = new Rate("openshard_errors");
const tunnelTime = new Trend("openshard_tunnel_duration", true);  // true = milliseconds

// ── Load scenario ─────────────────────────────────────────────────────────────
export const options = {
  stages: [
    { duration: "30s", target: 5         },  // warm-up
    { duration: "1m",  target: MAX_VUS   },  // ramp-up
    { duration: "2m",  target: MAX_VUS   },  // steady state
    { duration: "1m",  target: MAX_VUS*2 },  // stress peak
    { duration: "2m",  target: MAX_VUS*2 },  // hold peak
    { duration: "30s", target: 0         },  // ramp-down
  ],
  thresholds: {
    // Thesis pass/fail criteria
    "http_req_duration":    ["p(95)<2000"],  // 95% of requests under 2s
    "http_req_failed":      ["rate<0.05"],   // less than 5% errors
    "openshard_errors":     ["rate<0.05"],
  },
};

// ── Test logic ────────────────────────────────────────────────────────────────
export default function () {
  const res = http.get(TARGET_URL, {
    timeout: "10s",
    tags: { target: "openshard" },
  });

  const ok = check(res, {
    "status 200":        (r) => r.status === 200,
    "no server error":   (r) => r.status !== 503,
    "response time <2s": (r) => r.timings.duration < 2000,
  });

  errorRate.add(!ok);
  tunnelTime.add(res.timings.duration);

  sleep(1);
}

// ── Summary ───────────────────────────────────────────────────────────────────
export function handleSummary(data) {
  const reqs     = data.metrics.http_reqs?.values?.count ?? 0;
  const p95      = (data.metrics.http_req_duration?.values?.["p(95)"] ?? 0).toFixed(0);
  const p99      = (data.metrics.http_req_duration?.values?.["p(99)"] ?? 0).toFixed(0);
  const errRate  = ((data.metrics.http_req_failed?.values?.rate ?? 0) * 100).toFixed(2);
  const rps      = (data.metrics.http_reqs?.values?.rate ?? 0).toFixed(2);

  console.log("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
  console.log("  OpenShard Load Test — Summary");
  console.log("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
  console.log(`  Target:       ${TARGET_URL}`);
  console.log(`  Total reqs:   ${reqs}`);
  console.log(`  Avg req/s:    ${rps}`);
  console.log(`  p95 latency:  ${p95} ms`);
  console.log(`  p99 latency:  ${p99} ms`);
  console.log(`  Error rate:   ${errRate}%`);
  console.log("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

  return {
    "experiments/results/summary.json": JSON.stringify(data, null, 2),
  };
}
