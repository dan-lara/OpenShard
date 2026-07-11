use axum::{response::Html, response::IntoResponse};

pub async fn dashboard() -> impl IntoResponse {
    Html(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="refresh" content="10">
<title>OpenShard Dashboard</title>
<style>
  body { font-family: monospace; background: #111; color: #ddd; padding: 1rem; }
  h1 { color: #7cf; margin-bottom: 0.25rem; }
  .ts { color: #888; font-size: 0.85em; margin-bottom: 1rem; }
  table { border-collapse: collapse; width: 100%; }
  th { background: #222; color: #adf; text-align: left; padding: 6px 10px; border-bottom: 1px solid #444; }
  td { padding: 5px 10px; border-bottom: 1px solid #2a2a2a; vertical-align: top; }
  tr:hover td { background: #1a1a1a; }
  .alive { color: #5f5; }
  .dead  { color: #f55; }
  .svc   { color: #fa5; }
  .none  { color: #666; font-style: italic; }
  .running    { color: #5f5; }
  .notrunning { color: #f55; }
  .unknown    { color: #888; }
</style>
</head>
<body>
<h1>OpenShard Volunteers</h1>
<div class="ts" id="ts">Loading...</div>
<table id="tbl">
<thead>
<tr>
  <th>Hostname</th>
  <th>Status</th>
  <th>CPU %</th>
  <th>Mem %</th>
  <th>Weight</th>
  <th>Assigned Service</th>
  <th>Image</th>
  <th>Running</th>
</tr>
</thead>
<tbody id="body"></tbody>
</table>
<script>
async function load() {
  const resp = await fetch('/volunteers');
  const vols = await resp.json();
  document.getElementById('ts').textContent =
    'Last updated: ' + new Date().toLocaleTimeString() +
    ' — ' + vols.length + ' volunteer(s)';
  const tbody = document.getElementById('body');
  tbody.innerHTML = '';
  if (vols.length === 0) {
    tbody.innerHTML = '<tr><td colspan="8" class="none">No volunteers enrolled</td></tr>';
    return;
  }
  for (const v of vols) {
    const svc = v.assigned_service;
    const name  = svc ? svc.service_name : null;
    const image = svc ? svc.image        : null;

    let runClass, runLabel;
    if (v.service_running === true)       { runClass = 'running';    runLabel = 'yes'; }
    else if (v.service_running === false) { runClass = 'notrunning'; runLabel = 'no';  }
    else                                  { runClass = 'unknown';    runLabel = '—';   }

    const tr = document.createElement('tr');
    tr.innerHTML = `
      <td>${esc(v.info.hostname)}</td>
      <td class="alive">alive</td>
      <td>${v.metrics.cpu_pct.toFixed(1)}</td>
      <td>${v.metrics.mem_pct.toFixed(1)}</td>
      <td>${weight(v.metrics)}</td>
      <td>${name  ? '<span class="svc">'+esc(name)+'</span>'  : '<span class="none">—</span>'}</td>
      <td>${image ? '<span class="svc">'+esc(image)+'</span>' : '<span class="none">—</span>'}</td>
      <td class="${runClass}">${runLabel}</td>
    `;
    tbody.appendChild(tr);
  }
}

function weight(m) {
  const p = (m.cpu_pct * 0.5 + m.mem_pct * 0.3 + Math.min(m.active_requests, 20));
  return Math.max(1, Math.min(100, Math.round(100 - p)));
}

function esc(s) {
  return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}

load().catch(e => {
  document.getElementById('ts').textContent = 'Error: ' + e.message;
});
</script>
</body>
</html>
"#)
}
