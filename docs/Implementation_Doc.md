# OpenShard — Deep Architecture Handoff (v2)

**Audience:** an agent (or person) with *no prior context* who must use this for
any downstream purpose — TCC02 writing, a slide deck, or continued development.
This is the single source of truth for the system's shape and reasoning. Read it
top to bottom; you should be able to redraw the architecture diagram and explain
every design decision from this text alone.

> **Relationship to the original.** This is a refresh of
> `docs/architecture-handoff.md` (the diff baseline). Where a module is
> unchanged I say so and point back to the original's section ("see orig §X")
> rather than repeating it. Where something changed I explain it with the same
> *why-not-just-what* reasoning. **Two points were re-verified against the code
> and the experiment artifacts directly and are reported with exact citations:**
> multi-volunteer-per-service (§3.2 / §6) and the k6 results (§7).

> **Naming note.** Same as the original: the project is "OpenShard"; the live
> domain is spelled both `openshrd` and `openshard` (`*.openshrd.danlara.com.br`
> appears in code, `*.openshard.danlara.com.br` in the load-test scripts). Treat
> `openshard` as canonical; the mismatch is real.

### Reference documents (cite, don't duplicate)
- `docs/architecture-handoff.md` — the original handoff; the baseline for this doc.
- `docs/handoff-network-and-agent-rewrite.md` — network-exposure plan (public vs
  internal ports) and the planned Rust rewrite of the volunteer agent. Still
  accurate as *future work*; note its port-binding plan is **not yet
  implemented** (see §5).
- `README.md`, `PLAN.md`, `CLAUDE.md` — pitch/decisions/build commands.
- `experiments/load-test.js`, `experiments/load-test-all.js`,
  `experiments/run-report.sh`, `experiments/results/report.md`,
  `experiments/results/report-all.md` — the k6 harness and the two published
  result tables. The raw k6 dumps (`summary.json`, `summary-all.json`,
  `*.row.json`) back those tables; §7 cites the exact files.

---

## 1. Problem & motivation

**Unchanged from the original — see orig §1.** OpenShard is a volunteer-server
framework for distributed web hosting: pool the *idle* compute of ordinary
machines to host real websites, motivated as much politically (accessible
non-profit hosting, reduce cloud-oligopoly dependence, *without* crypto-DePIN
token incentives) as technically. The four hostile properties of volunteer
hardware that drive every design choice are identical: **behind NAT** (forces a
reverse tunnel), **unreliable/ephemeral** (forces heartbeats + churn + state
that survives a node vanishing), **weak/heterogeneous** (forces capacity-aware
weighting and the ability to *cap* a node for evaluation), and
**untrusted-ish** (single-operator lab; controller runs images on volunteer
Docker sockets — a documented liability). Mental model unchanged: *a public
front door that is always up, plus a fleet of disposable workers that phone
home.*

---

## 2. System overview (the 30-second mental model)

**Unchanged from the original — see orig §2.** One always-on **controller**
(Proxmox LXC) and **many volunteers** (Docker hosts anywhere). A visitor hits
**HAProxy :80**, which routes by `Host:` header to a per-service backend; that
backend points at a **local port on the controller** that is the controller-side
mouth of a **reverse tunnel** the volunteer opened. Volunteers **enroll** with
the **registrar**, receive a job ("run this image"), and send **heartbeats** that
keep them alive and re-tune their weight; a **churn** task evicts and re-queues
the job of any node that stops heartbeating. Supporting cast: a **Docker
registry**, **SQLite** persistence, and **Prometheus/Grafana**.

One clarification worth carrying into the deck: capacity-aware weighting governs
the shared `volunteers` backend, **not** the per-service path in steady state —
each `svc_*` backend holds exactly one server at a fixed weight (see §3.2/§6).

---

## 3. Modules

Each subsection: *responsibility · why separate · key decisions and reasoning ·
interface · current state.* Modules that did not change materially are
summarized in one line with a pointer back to the original.

### 3.1 Registrar — `src/core-server/registrar/`

**Mostly unchanged from the original — see orig §3.1** for the full treatment
(control plane on Axum :3000; capacity-aware weight; stable identity keyed on
hostname; SQLite persist + two-phase boot recovery; churn re-queue). The weight
formula is **verified identical**:
`weight = 100 − (cpu·0.5 + mem·0.3 + min(active_requests, 20))`, clamped to
`[1,100]` (`state.rs::Metrics::weight`, lines 43–46). Churn threshold is
**verified**: a node is evicted after 45 s without a heartbeat
(`HEARTBEAT_TIMEOUT_SECS = 45`, checked every 15 s — `state.rs:12`, `churn.rs:7`).

**What changed:**

- *The heartbeat response now also carries the assignment* (`enrollment.rs`
  `heartbeat`, lines 326–335): every `/heartbeat` reply includes
  `{"assignment": {image, service_port}}` when the volunteer has one. This is a
  belt-and-suspenders addition so a volunteer that was assigned a service
  *after* it enrolled (via `POST /services` landing on an already-running node)
  picks the job up on its next 15 s tick, instead of only at enroll time. Enroll
  still returns the assignment too (`EnrollResponse.assignment`).

- *Two assignment entry points, both strictly 1:1.* `POST /services`
  (`services.rs::register_service`) finds **one** free volunteer
  (`.find(|v| v.assigned_service.is_none())`, line 38) and assigns it, or queues
  the service as pending if none is free. Enrollment
  (`enrollment.rs::enroll`) dequeues **one** pending service per volunteer
  (`pop_front`, line 186). There is no path that attaches a second volunteer to
  an existing service — see §3.2 and §6 for why this matters.

- *`active_volunteers()` is now weight-sorted* (`state.rs:119–121`, descending by
  `weight()`). So "the free volunteer" picked by `register_service` is the
  highest-weight free one, and the legacy `/dispatch` proxy picks the
  highest-weight node overall.

- *Vestigial endpoints.* `POST /volunteers/:id/tunnel-port`
  (`enrollment.rs::update_tunnel_port`) and `POST /dispatch`
  (`dispatch.rs`) both still exist and are wired in `main.rs`, but neither is on
  the live data path. `dispatch` is an explicitly-temporary direct-HTTP
  forwarder ("NOTE: temporary direct HTTP forward … once the tunnel is ready
  this will forward through the tunnel"); the real path is HAProxy → tunnel.
  `tunnel-port` is made redundant by the per-stream address read in the tunnel
  client (orig §3.3).

**Interface.** HTTP/JSON, unchanged set: `POST /enroll`, `POST /heartbeat`,
`DELETE /enroll/:id`, `GET /volunteers`, `POST /volunteers/:id/tunnel-port`,
`POST /services`, `GET /services`, `GET /metrics`, `GET /` (dashboard),
`POST /dispatch` (`main.rs:130–141`). Calls `haproxy-manager` in-process;
reads/writes SQLite; resolves `TUNNEL_HOST` to the tunnel IP.

**Current state.** Solid and demonstrable, same as the original.

### 3.2 HAProxy + `haproxy-manager` — `src/core-server/haproxy-manager/`

**Mostly unchanged from the original — see orig §3.2** for the two-subsystem
split (`runtime.rs` = zero-downtime server changes over the admin socket;
`config.rs` = backend/ACL edits to `haproxy.cfg` + validate + reload), the
serialized-reload mutex with a 750 ms settle (`config.rs:14–21,128,166`), and
the `set addr`-then-`add server` upsert fix (`lib.rs::upsert_server`,
lines 230–246). All verified present and as described.

**What changed / what to be precise about:**

- **One server per service, fixed name `primary`, fixed weight 100 — verified.**
  When a service is assigned, `lib.rs::assign_service` (lines 130–150) creates
  `backend svc_<name>` + the host ACL via `config::add_service`, then calls
  `upsert_svc_server`, which upserts a **single** server literally named
  `"primary"` at weight `100` (`lib.rs:219–222`). Re-assignment *replaces* that
  one server. The backend block declares `balance leastconn` (`config.rs:94–98`)
  but with only ever one server, the balancing policy is moot.

- **There is no `backup` mechanism and no near-equal-weight round-robin.**
  `grep -rin "backup"` across the source matches only the words "restore
  backup" in `config.rs` error handling — **no HAProxy `backup` server flag is
  ever emitted**. There is no second/standby server, no threshold comparing two
  volunteers' weights, no round-robin between near-equal nodes. See §6 for the
  honest framing.

- **The capacity-aware weight never reaches a `svc_*` backend in steady state.**
  `heartbeat` only calls `set_weight("volunteers", …)` (`enrollment.rs:309`);
  the `svc_*` `primary` stays pinned at 100. So dynamic weighting shapes traffic
  only on the shared default `volunteers` backend, not on the per-service path.

- **New `subdomain.rs` module — present but NOT wired in.** `haproxy-manager`
  gained a `subdomain` module (`lib.rs:3`) with a `DnsProvider` trait, a
  `NoopProvider`, and a `CloudflareProvider` (Cloudflare API v4 A-record
  create/delete from env vars `CLOUDFLARE_TOKEN`/`CLOUDFLARE_ZONE_ID`/
  `BASE_DOMAIN`/`SERVER_IP`). **`grep` confirms the registrar never imports or
  calls it** ("NONE in registrar"), and `docker-compose.yml` leaves the
  Cloudflare vars commented out (lines 37–41). Its own doc-comment says the
  wildcard `*.openshrd.danlara.com.br` is already covered by a Cloudflare Tunnel,
  so per-subdomain records aren't needed. **Treat it as scaffolding for the
  deferred "wildcard/automated DNS" item, not a live feature.** (This addition
  pulled `reqwest` into the crate's `Cargo.toml`.)

**Interface.** Typed Rust API up (`add_volunteer`, `upsert_volunteer`,
`set_weight`, `remove_volunteer`, `assign_service`, `restore_service_server`,
`ensure_service_config`, `update_volunteer_addr`, `list_services`); admin socket
+ `haproxy.cfg`/SIGUSR2 down; HAProxy health-checks each `svc_*` with
`option httpchk GET /` expecting `rstatus 2[0-9][0-9]` (`config.rs:96`).

**Current state.** Working for the demonstrated single-volunteer-per-service
path. The reload-vs-runtime-server interaction (§6) remains the main fragility.

### 3.3 Reverse tunnel — `src/core-server/tunnel/server.rs`, `src/shard-node/container/client.rs`, shared `proto.rs`

**Unchanged from the original — see orig §3.3.** Control port **9007**, public
+ data ports from the **30000–31000** pool, 4-byte handshake, `PING`/`OPEN`/
`CLOSE` framing, splice-on-`OPEN`. The headline decision is **verified still in
place**: the client reads `LOCAL_SERVICE_ADDR` **per stream**, not once per
session (`client.rs:34–43` resolve fn, called from `handle_stream`,
lines 123/135), with priority env var → `/tmp/tunnel_local_addr` file →
`127.0.0.1:8080`. That is what keeps a node's public port stable for life and
makes the registrar's `tunnel-port` endpoint vestigial.

**Current state.** Works end-to-end (unchanged).

### 3.4 Volunteer node — `src/shard-node/container/`

**Unchanged in substance from the original — see orig §3.4.** Verified against
`agent.py`:

- Talks to Docker over the socket via **raw stdlib HTTP**, no `docker` CLI
  (`agent.py` `_docker`, lines ~135–163; socket `/var/run/docker.sock`).
- Runs the assigned image with **no published ports**, reads the container's
  internal Docker IP, and writes `LOCAL_SERVICE_ADDR = <container_ip>:<port>` to
  the file the tunnel client reads — **no client restart** needed
  (`run_service` lines 197–227, `apply_service_assignment` lines 247–261; the
  comment at 250–252 spells out the "re-read per stream → just write the file"
  contract).
- Honors **resource caps** `SERVICE_CPUS`/`SERVICE_MEMORY` as
  `NanoCpus`/`Memory`+`MemorySwap` on the spawned container
  (`_service_host_config`, lines 55–69) — this is what makes the capped-node
  evaluation real.
- Reports **`service_running`** (and `service_image`) in each heartbeat
  (`heartbeat_loop`, line 372; `_service_is_running` inspects the container,
  lines 236–245). The registrar surfaces this on the dashboard.
- Metrics from `/proc`; on a 404 heartbeat (registrar lost state) it re-enrolls
  (lines ~388–390). Trust model unchanged (mounts host Docker socket).

**Current state.** Works: enroll, heartbeat, assignment apply (pull+run+cap),
churn re-enroll with identity reuse.

### 3.5 Docker registry — `registry:2`

**Unchanged — see orig §3.5.** Plain HTTP on `:5000`, internal, every puller
needs `insecure-registries`. TLS+auth deferred. Defined in
`docker-compose.yml:83–89`.

### 3.6 Persistence — SQLite (registrar)

**Unchanged — see orig §3.6.** Two tables, `volunteers` and
`service_assignments` (`db.rs:12,34`); orphan-pruning on boot
(`prune_orphan_assignments`); recovery via `load_all`. All present and verified.

### 3.7 Monitoring — `monitoring/` + registrar `/metrics`

**Unchanged — see orig §3.7.** Prometheus scrapes HAProxy's exporter on `:8405`
and node-exporter (`docker-compose.yml:53–67`); registrar exposes custom
counters/gauges at `GET /metrics` (enrollments, heartbeats, evictions, active
volunteers, per-host weight/cpu/mem — see the `metrics::*` calls in
`enrollment.rs`/`churn.rs`).

---

## 4. How the modules connect (redraw the diagram from this)

**Unchanged from the original — see orig §4** for the topology and edge list.
Two small corrections against the current `docker-compose.yml`:

- The controller stack is **registrar (HAProxy in the same container) + tunnel
  (host network) + registry + node-exporter** (`docker-compose.yml`). The
  registrar reaches the tunnel via `TUNNEL_HOST: tunnel` resolved through
  `extra_hosts: tunnel:host-gateway` (lines 34–36), i.e. the host gateway, not a
  compose-internal DNS name.
- **Ports are still published on all interfaces** (`80:80`, `3000:3000`,
  `8405:8405`, `5000:5000`) — the `10.10.10.10`-only binding from
  `docs/handoff-network-and-agent-rewrite.md` is **not implemented yet** (§5).

Failure behavior is unchanged (orig §4): volunteer dies → churn evicts after
45 s and re-queues its service → brief 503 for that one site → self-heal on
re-enroll; service container crash → `--restart` + health-check 503 meanwhile;
controller restart → two-phase SQLite recovery; empty `svc_*` backend → 503 for
that host (no fall-through to `default_backend`).

---

## 5. End-to-end flows

**Unchanged from the original — see orig §5** for Flow A (visitor request),
Flow B (volunteer joins and starts hosting), and Flow C (volunteer dies and the
site recovers). All three were re-checked against the code and still hold, with
one nuance to fold into Flow B: a service registered while a node is *already
running* is delivered on the **next heartbeat** (the assignment now rides the
heartbeat response, §3.1), not only at enroll.

**Not-yet-done network hardening (was "future" in the original, still future):**
binding back-office ports to an internal interface only, and `BIND_ADDR` for the
tunnel server, are designed in `docs/handoff-network-and-agent-rewrite.md` but
not in the compose file. Remote volunteers over the VPN remain not-turnkey
(orig §6).

---

## 6. Current state — demonstrable vs. partial (be honest)

**Demonstrable end-to-end (unchanged from orig §6, all re-verified):**
- Service registration → assignment → image pull → capped container run →
  HAProxy routing → live response, across multiple distinct services on
  multiple volunteers.
- **Stable identity across a full redeploy** (the headline correctness fix).
- **Resource-capped node evaluation with k6** — exact numbers in §7.
- Controller crash recovery; churn + re-queue; capacity-aware weighting of the
  shared backend; live Prometheus/Grafana.

**Partial / fragile / deferred:**

- **Still one volunteer per service (v1) — verified, no change.** The original
  listed this as deferred; it is *still* deferred. The per-service backend holds
  a single fixed `primary` server (§3.2). There is **no** "highest-weight node
  serves, others sit as HAProxy `backup`, round-robin between near-equal
  weights" mechanism — no `backup` flag is emitted anywhere, and no
  near-equal-weight threshold exists in the code. Multi-volunteer-per-domain
  load balancing remains future work. *(If a slide or section claims active/
  standby or weight-banded round-robin per service, it is wrong as of this
  codebase.)*
- **Reloads still wipe runtime-added servers** (orig §6). Registering a *new*
  service triggers a reload; runtime-added servers (not in the file) are lost
  until re-added (guaranteed only on restart). The serialized-reload mutex
  prevents *dropped* reloads, not this wipe. Operational rule unchanged: don't
  register services during a live demo; register, restart once, then run.
- **Network exposure not hardened** — ports still on all interfaces (§4/§5).
- **Subdomain/DNS automation is scaffolding only** — `subdomain.rs` exists but is
  not wired into the registrar and the Cloudflare vars are unset; the wildcard is
  handled out-of-band by a Cloudflare Tunnel (§3.2). Counts as partial, not done.
- **Weight doesn't reflect cgroup caps** (host `/proc`) — cosmetic, unchanged.
- **Trust model wide open** (host Docker socket + controller-supplied images) —
  unchanged.
- **Rust agent rewrite** designed, not built —
  `docs/handoff-network-and-agent-rewrite.md`.
- **Vestigial code paths** (`/dispatch` direct forwarder, `/volunteers/:id/
  tunnel-port`) still present; harmless but not the live path (§3.1).

---

## 7. Experiment data — exact, final k6 numbers

There are **two published reports**, from two different harnesses, plus one raw
dump that is *not* reflected in either report (flagged below). Numbers are quoted
verbatim from the artifacts — not rounded or estimated.

### 7.1 Two-service run @ 200 VUs — `experiments/results/report.md`

Source rows: `svc-a__sem_limite_.row.json`, `svc-pc__0.5_CPU___256MB_.row.json`
(produced by `experiments/run-report.sh` → `experiments/load-test.js`). Note
these rows record **avg / p95 / p99** but **no p50/median**.

| Service | VUs | avg | p95 | p99 | error rate | throughput | requests |
|---|---|---|---|---|---|---|---|
| svc-a (sem limite / **uncapped**) | 200 | 18 ms | **55 ms** | 58 ms | **0.00 %** | 167.1 req/s | 14 331 |
| svc-pc (**0.5 CPU / 256 MB**, capped) | 200 | 33 ms | **63 ms** | 76 ms | **0.00 %** | 164.5 req/s | 14 119 |

Objective in the harness: p95 < 300 ms, errors = 0 %. Both pass. The capped node
costs **~1.15× the p95** of the uncapped one (63 vs 55 ms) and ~1.8× the average
(33 vs 18 ms) at this load — still comfortably inside the 300 ms budget with zero
errors. *(The "~4× p95" phrasing in the original handoff is not supported by these
final rows; the measured gap is ~1.15× at 200 VUs.)*

### 7.2 Four-service run @ 120 VUs, ~6.7 min — `experiments/results/report-all.md`

Source: `experiments/results/summary-all.json` (k6 raw; `experiments/
load-test-all.js`). Test run duration **400.16 s**, 120 VUs, **133 973** total
requests at **334.8 req/s**, **0 failed requests** (`http_req_failed` rate 0,
`checks` 133 973/0). Per-service trends (p50 = k6 `med`):

| Service | avg | p50 (med) | p95 | p99 | max | error rate |
|---|---|---|---|---|---|---|
| svc-a (uncapped) | 39.24 ms | 42.94 ms | **58.11 ms** | 67.17 ms | 109.39 ms | **0 %** |
| svc-b (uncapped) | 37.23 ms | 42.40 ms | **57.55 ms** | 66.01 ms | 104.42 ms | **0 %** |
| svc-index (uncapped) | 40.47 ms | 43.24 ms | **57.21 ms** | 67.69 ms | 127.84 ms | **0 %** |
| svc-pc (**0.5 CPU / 256 MB**) | 39.79 ms | 44.25 ms | **58.40 ms** | 71.41 ms | 114.88 ms | **0 %** |

Aggregate `http_req_duration`: avg 39.18 ms, p50 43.18 ms, p95 57.81 ms,
p99 68.01 ms, max 127.84 ms; threshold `p(95)<300` passed
(`summary-all.json:288–305`). At this concurrency the capped node is
**statistically indistinguishable** from the uncapped ones on p50/p95 (within
~1 ms), only pulling slightly ahead at the p99 tail (71.4 vs ~66–68 ms). The
`report-all.md` table rounds these to integers (e.g. p95 = 58/58/57/58 ms).

### 7.3 Raw dump not in either report — `experiments/results/summary.json` (flag)

`summary.json` is a separate, earlier raw k6 dump with **different parameters**
(100 VUs, 420.49 s, 23 601 requests) and does **not** correspond to either table
above. It is the **only** artifact showing any errors, and they are tiny:
`http_req_failed` rate **0.038 %** (9 of 23 601), a custom `openshard_errors`
rate **0.16 %** (38), one ~10.0 s outlier (`max` 10 001 ms — a timeout), and an
aggregate p95 of **164.5 ms**. It carries an `openshard_tunnel_duration` metric,
suggesting an earlier tunnel-stress run. **Do not cite it as a current result;**
mention it only if you specifically want to show a near-zero-but-nonzero error
case under a single tail timeout.

**Bottom line for the deck/thesis:** across the final reported runs (200 VUs/2
services and 120 VUs/4 services), OpenShard served **0 % errors** and **p95
≤ 63 ms** against a 300 ms objective, with a 0.5-CPU/256-MB capped node tracking
uncapped nodes closely — concrete evidence the system works on weak hardware at
measurable, small cost.

---

## 8. Suggested next steps (for whoever continues the work)

Ordered roughly by leverage; the first two are the honest gaps a reviewer will
poke at.

1. **Make per-service hosting actually multi-volunteer.** This is the single
   biggest deferred feature and the one most likely to be assumed already done.
   A minimal version: allow N volunteers to attach to one `svc_<name>` backend
   (drop the fixed `primary` name → use the per-volunteer server name already
   produced by `server_name_for`), push the dynamic weight to the `svc_*`
   backend on heartbeat (today it only updates `volunteers`), and — if an
   active/standby model is wanted — emit the HAProxy `backup` flag for all but
   the top-weight server, with an explicit weight threshold for when peers are
   "near-equal" and should round-robin instead. None of this exists today.
2. **Fix the reload-wipes-runtime-servers gap** (orig §6): re-add all runtime
   servers after every config reload, or pin them in the file. Removes the
   "don't register during a demo" caveat.
3. **Wire or remove `subdomain.rs`.** Either call `DnsProvider` from the service
   lifecycle (registrar) so subdomains are managed automatically, or delete it to
   avoid implying a feature that isn't connected.
4. **Harden network exposure** per `docs/handoff-network-and-agent-rewrite.md`
   (internal-only binds, tunnel `BIND_ADDR`), and make remote/VPN volunteers
   turnkey.
5. **Retire the vestigial paths** (`/dispatch`, `/volunteers/:id/tunnel-port`)
   once their absence is confirmed safe, to shrink the surface a new reader has
   to reason about.
6. **Build the Rust agent rewrite** (single `FROM scratch` binary, ~15–20 MB)
   once the above is stable — design is ready in the network/agent handoff.
7. **Registry TLS + auth** before any third-party enrollment; the trust model
   (host Docker socket) must change in the same breath.
