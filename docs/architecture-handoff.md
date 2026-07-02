# OpenShard — Deep Architecture Handoff

**Audience:** an agent (or person) with *no prior context* who must produce a TCC02
defense slide deck. This document is the single source of truth for the system's
shape and reasoning. Read it top to bottom; you should be able to redraw the
architecture diagram and explain every design decision from this text alone.

> **Naming note.** The project is "OpenShard." The *live* deployment uses the
> domain spelled `openshrd` (no `a`) in some places and `openshard` in others —
> this inconsistency is real and is called out in `PLAN.md` (top section). Treat
> `openshard` as canonical; mention the mismatch only if asked.
>
> **Redaction.** Concrete addresses are replaced with placeholders:
> `<CONTROLLER_IP>` (controller's internal lab IP), `<PROXMOX_TS_IP>` (Proxmox
> host's VPN/Tailscale IP), `<BASE_DOMAIN>` (the routed apex domain),
> `<REGISTRY>` = `<CONTROLLER_IP>:5000`. Lab container IDs (LXC 102/111–113/200)
> are kept because they aid the mental model and are not secrets. No keys/tokens
> are reproduced here; Cloudflare credentials are referenced only by env-var name.

### Reference documents (do not duplicate — cite these)
- `README.md` — project pitch, authors, advisors, institution (UNIFEI).
- `docs/tcc1/Apresentacao_TCC1.pdf`, `docs/tcc1/Plano_de_trabalho_TCC01.pdf` —
  TCC1 (prior phase): problem framing and work plan. Use for the "where we came
  from" slide.
- `PLAN.md` — the service-assignment design (Option C), committed design
  decisions, and the explicit **Open Questions / deferred** list. The single best
  source for "what we chose and what we punted."
- `docs/handoff-network-and-agent-rewrite.md` — the network-exposure plan (which
  ports are public vs internal and why) and the planned Rust rewrite of the
  volunteer agent (with image-size budget). Cite for "future work."
- `CLAUDE.md` — build/run/deploy commands, env vars, deployment target.
- `experiments/load-test.js`, `experiments/load-test-all.js`,
  `experiments/results/report.md`, `experiments/results/report-all.md` —
  the k6 evaluation harness and results. Use for the "results" slide.
- `scripts/redeploy.sh` — the end-to-end build+distribute+restart mechanism.

---

## 1. Problem & motivation

OpenShard is a **volunteer-server framework for distributed web hosting**: it lets
people donate the *idle* compute of ordinary machines (lab PCs at a university, a
spare desktop, a home server) and pools that capacity to host real websites. The
thesis goal (`README.md`) is explicitly *political-economic* as much as technical:
provide accessible, collaborative hosting for non-profit projects and reduce
dependence on the cloud oligopoly (AWS/Azure), **without** the speculative,
token-incentive model of crypto DePINs. The contributor's payment is goodwill and
spare cycles, not a coin.

That motivation forces almost every architectural choice, because volunteer
hardware is **hostile to the assumptions commercial hosting makes**:

- **Volunteers are behind NAT / firewalls.** A donated PC in a dorm has no public
  IP and no port-forwarding. So the system cannot "connect to" a volunteer — the
  volunteer must dial *out* to the controller and hold the connection open. This
  is the single fact that makes a **reverse tunnel** non-negotiable (see §3.3).
- **Volunteers are unreliable and ephemeral.** They reboot, get shut off, lose
  network. So the controller needs continuous liveness tracking (heartbeats +
  churn eviction) and must treat any node as disposable. Hosting state must
  survive a node vanishing.
- **Volunteers are weak and heterogeneous.** Half a core here, a Raspberry Pi
  there. So load balancing must be *capacity-aware* (a dynamic weight derived
  from live CPU/RAM), not round-robin, and the evaluation must be able to *cap*
  resources to model a realistic weak donor.
- **Volunteers are untrusted-ish.** For v1 this is a single-operator lab, so the
  trust model is deliberately loose (the controller runs arbitrary images on
  volunteer hosts via their Docker socket — see §3.4 and the "Trust assumption"
  in `PLAN.md`). This is a known, documented liability, not an oversight.

So the architecture is best understood as **"a public front door that is always
up, plus a fleet of disposable workers that phone home."** The controller is the
stable, public, trusted core; everything volunteer-side is transient and reached
only through tunnels the volunteers themselves opened.

---

## 2. System overview (the 30-second mental model)

There is **one controller** (a small, always-on server in a Proxmox LXC) and
**many volunteers** (Docker hosts that can be anywhere).

A visitor hits the controller's **HAProxy** on port 80. HAProxy looks at the
`Host:` header and routes to the backend for that site. That backend doesn't point
at the volunteer directly (it can't — NAT); it points at a **local port on the
controller** that is the controller-side mouth of a **reverse tunnel** the
volunteer opened earlier. Bytes flow controller → tunnel → volunteer → the
**service container** the volunteer is running for that site, and back.

Volunteers announce themselves to the **registrar** (an HTTP API on the
controller). On enrollment the registrar hands the volunteer a job ("run this
Docker image"), wires up the HAProxy backend, and thereafter receives
**heartbeats** that keep the node alive and re-tune its load-balancing weight. If
heartbeats stop, a **churn** task evicts the node and re-queues its job for the
next volunteer.

Supporting cast: a **Docker registry** on the controller distributes the service
images; **SQLite** persists volunteer/assignment state so a controller restart
recovers; **Prometheus + Grafana** scrape and visualize metrics.

---

## 3. Modules

Each subsection: *responsibility · why it's a separate thing · key decisions and
the reasoning · interface to others · current state.*

### 3.1 Registrar — `src/core-server/registrar/`

**Responsibility.** The control plane. An Axum HTTP server (port 3000) that owns
the lifecycle of every volunteer: enrollment, heartbeats, liveness/churn,
service registration and assignment, the operator dashboard, and Prometheus
metrics. It is the *only* component that holds the authoritative model of "which
volunteer is running which service, and how healthy is it."

**Why separate.** It is the brain, and it must be the most stable, trusted piece.
Keeping all orchestration logic in one Rust service (rather than scattered scripts
or baked into HAProxy) means there is exactly one place that decides assignment
and weight, and one place that persists state.

**Key decisions & reasoning.**
- *Capacity-aware weight, not round-robin.* On each heartbeat the registrar
  computes `weight = 100 − (cpu·0.5 + mem·0.3 + min(active_requests,20))`, clamped
  to `[1,100]` (`state.rs::Metrics::weight`), and pushes it to HAProxy. A loaded
  or weak node automatically receives less traffic. This is the direct technical
  answer to "volunteers are heterogeneous."
- *Stable identity keyed on the node's hostname.* This is the most important
  correctness decision and was a hard-won fix. Originally every `/enroll` minted a
  fresh `Uuid::new_v4()`; because assignments (and HAProxy server names) are keyed
  by that UUID, **every volunteer restart/redeploy orphaned its assignment** and
  left dead rows. The registrar now reuses the UUID — and re-attaches the existing
  assignment — of any volunteer already known under the same agent-supplied
  hostname (the "node key"), looked up first in memory then in SQLite
  (`enrollment.rs`, `db.rs::find_by_hostname`/`get_assignment`). The deploy sets a
  stable `HOSTNAME` per volunteer, so identity now survives restarts. *This is why
  a redeploy no longer breaks hosting* and is worth a slide.
- *Persist to SQLite, recover on boot.* Volunteer rows and `service_assignments`
  live in SQLite (`db.rs`). On startup the registrar replays them, **waits for the
  HAProxy admin socket to exist** (it isn't ready the instant the process starts —
  a real race we hit), prunes assignment rows whose volunteer is gone, then
  re-adds servers via the runtime API (`main.rs`). Recovery is two-phase: all
  config reloads first, then all runtime server additions, so a reload can't wipe
  a server added moments earlier.
- *Churn re-queues, not just evicts.* When a node misses heartbeats for 45s
  (`churn.rs`, checked every 15s), it's removed from memory, SQLite, and HAProxy —
  and if it had a service, that service is pushed back onto a `pending_services`
  queue so the next enrolling volunteer picks it up. This is how hosting survives
  a node dying.

**Interface.** HTTP/JSON. Endpoints: `POST /enroll`, `POST /heartbeat`,
`DELETE /enroll/:id`, `GET /volunteers`, `POST /volunteers/:id/tunnel-port`,
`POST /services`, `GET /services`, `GET /metrics`, `GET /` (dashboard),
`POST /dispatch`. Downward it calls `haproxy-manager` functions directly (same
process space, library crate). It reads/writes SQLite. It resolves `TUNNEL_HOST`
to compute the controller-local tunnel address used as the HAProxy backend target.

**Current state.** Solid and demonstrable: enrollment, heartbeat/weight,
assignment-at-enroll and assignment-at-registration, stable identity across
redeploys, churn+re-queue, crash recovery, dashboard, Prometheus metrics.

### 3.2 HAProxy + `haproxy-manager` — `src/core-server/haproxy-manager/`

**Responsibility.** The data-plane front door and its programmatic controller.
HAProxy terminates port 80 and routes by `Host:` header. `haproxy-manager` is a
**library crate** (used in-process by the registrar) that manipulates HAProxy two
different ways, deliberately split:
- `runtime.rs` — talks to HAProxy's **Unix admin socket** (`admin.sock`) to
  add/remove/weight/re-address *servers* with **zero downtime, no reload**.
- `config.rs` — **edits `haproxy.cfg` on disk**, validates with `haproxy -c`, and
  reloads via `SIGUSR2`, used to add/remove *backends + frontend ACLs* (which the
  runtime API cannot create).

**Why separate.** Two reasons. (1) Volunteer churn is constant and must be
zero-downtime, so server changes go through the socket and never touch the file.
Backend/ACL creation is rare and *must* go through the file (HAProxy can't define
a new backend at runtime), so it's isolated and reload-guarded. (2) Putting the
HAProxy mechanics behind a typed Rust API keeps the registrar's logic clean and
testable.

**Key decisions & reasoning.**
- *The baked `haproxy.cfg` is minimal* (`haproxy-manager/haproxy.cfg`): a
  `frontend` on `:80` with `default_backend volunteers`, a `volunteers` backend
  (`balance leastconn`), and a Prometheus frontend on `:8405`. Everything
  per-service is added dynamically. `leastconn` (not round-robin) complements the
  weight system for uneven nodes.
- *Per-service backend with a single fixed server name `primary`.* When a service
  is assigned, `assign_service` creates `backend svc_<name>` + an
  `acl host_<name> hdr(host) -i <domain>` + `use_backend`, then upserts one server
  named `primary` pointing at the volunteer's tunnel address. The fixed name means
  re-assignment *replaces* rather than *accumulates* servers across restart cycles.
- *`set addr` first, `add server` as fallback.* A subtle but critical fix: HAProxy
  replies to a duplicate `add server` with text that the socket layer does **not**
  flag as an error, so the old "add first" path silently failed to repoint a
  re-enrolling volunteer. Upserts now try `set addr` (which errors cleanly with
  "No such server" when absent) and only then `add server`.
- *Serialized reloads.* Config edits now hold a mutex and sleep ~750 ms after each
  `SIGUSR2` (`config.rs`). This exists because back-to-back reloads during recovery
  were being **coalesced/dropped** by the HAProxy master, leaving a backend in the
  file but absent from the running process (a real bug that produced 503s for one
  service). See the gap in §6 about reloads also wiping runtime servers.

**Interface.** Upward: a typed Rust API (`add_volunteer`, `upsert_volunteer`,
`set_weight`, `remove_volunteer`, `assign_service`, `update_volunteer_addr`,
`ensure_service_config`, `list_services`). Downward: the HAProxy admin socket and
the `haproxy.cfg` file + `SIGUSR2`. Sideways: HAProxy health-checks each
`svc_<name>` backend with `option httpchk GET /` expecting a 2xx.

**Current state.** Working for the demonstrated path. The reload-vs-runtime-server
interaction (§6) is the main remaining fragility.

### 3.3 Reverse tunnel — `src/core-server/tunnel/server.rs`, `src/shard-node/container/client.rs`, shared `proto.rs`

**Responsibility.** Move bytes between the controller and a NAT'd volunteer over a
connection the *volunteer* initiated. This is the mechanism that makes hosting on
firewalled hardware possible at all.

**Why separate / why it exists.** Restated from §1: volunteers can't accept inbound
connections, so a classic "load balancer → backend IP" model is impossible. The
volunteer dials out and the controller multiplexes visitor traffic back down that
pipe. It's a small bespoke protocol rather than an off-the-shelf tool (e.g. frp,
ngrok) to keep the dependency surface tiny and the thesis self-contained.

**Protocol (enough to redraw it).** The **server** (controller, host-network,
control port **9007**, port pool **30000–31000**) owns *all* allocation. When a
client connects:
1. Server allocates a **public port** (HAProxy will target this) and a **data
   port** (the client dials back here per visitor), both from the pool, binds
   both, and sends a 4-byte handshake `[public_hi, public_lo, data_hi, data_lo]`.
   The client sends nothing during the handshake — it only reads.
2. The client writes the public port to `/tmp/tunnel_public_port` so the agent can
   include it in enrollment.
3. Control channel uses fixed-size frames (`proto.rs`): the server sends
   `PING` periodically and `OPEN(stream_id)` when a visitor arrives; the client
   replies `PONG` and acts on `OPEN`. `CLOSE` tears a stream down.
4. On `OPEN(stream_id)`, the client dials the server's **data port**, writes the
   2-byte `stream_id`, then connects to its **local service** and splices the two
   sockets. The server matches the incoming data connection to the waiting visitor
   by `stream_id` and splices visitor ↔ agent-data.
5. On session end the server releases both ports; the client reconnects after a
   delay (and will receive *new* ports).

**Key decision (and a fix).** The client reads its local service address
(`LOCAL_SERVICE_ADDR`, default `127.0.0.1:8080`) **per stream**, not once per
session (`client.rs::handle_stream`). This was changed deliberately: previously
the agent had to *restart* the tunnel client to point it at a newly-started
service container, which allocated a *new* public port and forced a fragile
"tell-the-registrar-my-new-port" dance (`POST /volunteers/:id/tunnel-port`) that
raced and produced 503s. Re-reading per stream means the agent just rewrites a
file and the next request picks up the new target — **the public port stays
stable for the node's whole life**, and the HAProxy backend never needs repointing.

**Interface.** Server ↔ client over raw TCP (9007 control + a per-session data
port). Client ↔ local service over TCP to `LOCAL_SERVICE_ADDR`. Client ↔ agent via
the `/tmp/tunnel_public_port` and `/tmp/tunnel_local_addr` files. HAProxy ↔ tunnel
via `127.0.0.1:<public_port>` on the controller.

**Current state.** Works end-to-end. The `POST /volunteers/:id/tunnel-port`
endpoint still exists but is now effectively vestigial given the per-stream read.

### 3.4 Volunteer node — `src/shard-node/container/`

**Responsibility.** Be a disposable worker. Open the tunnel, enroll, heartbeat,
and — when assigned — pull and run the service's Docker image and expose it to the
tunnel.

**Composition.** One Docker image (`openshard/volunteer`, Alpine, ~74 MB) running
three things via `entrypoint.sh`: (1) the compiled Rust **tunnel client**, (2) an
**nginx placeholder** on `SERVICE_PORT` (8080) that serves a "Volunteer Node" page
until a real service is assigned, and (3) the Python **agent** (`agent.py`).

**Key decisions & reasoning.**
- *The agent talks to Docker over the socket via raw HTTP from stdlib*, not the
  `docker` CLI — this shaved the image down and removed a dependency
  (`docs/handoff-network-and-agent-rewrite.md` documents the size budget).
- *Service containers publish no ports.* The agent runs the assigned image with
  no `-p`, reads the container's internal Docker IP via inspect, and sets
  `LOCAL_SERVICE_ADDR = <container_ip>:<port>`. This is the design that lets
  **multiple volunteer containers coexist on one host** without port collisions
  (see `PLAN.md` scaling model) — the alternative fixed-port scheme collided.
- *Resource caps on the service container.* The agent honors `SERVICE_CPUS` /
  `SERVICE_MEMORY` env vars and applies them as `NanoCpus`/`Memory` on the
  spawned container (`agent.py::_service_host_config`). This is what makes the
  evaluation *realistic*: you can model a 0.5-CPU / 256-MB donor and measure it.
- *Metrics come from `/proc`.* CPU/mem/load are read directly. Caveat for the
  deck: under a cgroup cap the `/proc` numbers still reflect the host, so the
  *weight* won't mirror the cap — but the *service container is genuinely capped*,
  so latency/throughput results are real.
- *Trust:* the agent mounts the host Docker socket and runs controller-supplied
  images → the controller has root-equivalent control of every volunteer host.
  Acceptable for a single-operator lab, flagged in `PLAN.md`.

**Interface.** Up to the registrar via HTTP (`/enroll`, `/heartbeat`). To the
tunnel client via `/tmp` files. To Docker via the host socket. Pulls images from
`<REGISTRY>` (plain HTTP → requires `insecure-registries` on the host daemon).

**Current state.** Works: enroll, heartbeat, assignment apply (pull+run+cap),
churn re-enroll with identity reuse.

### 3.5 Docker registry — `registry:2` (controller `docker-compose.yml`)

**Responsibility.** Distribute service images (and the volunteer image) to nodes.
Plain HTTP on port 5000, internal-only; every host that pulls must list
`<REGISTRY>` in `insecure-registries` and restart its daemon. Owners push with
`docker tag … <REGISTRY>/<name>:latest && docker push …`. TLS + auth are
explicitly deferred (`PLAN.md` open questions).

### 3.6 Persistence — SQLite (registrar)

Two tables (`db.rs`): `volunteers` (identity, static info, last metrics,
timestamps) and `service_assignments` (volunteer_id → service name/domain/image/
port). It exists so a controller restart **recovers** the fleet view and hosting
assignments rather than starting blank. Keyed design and the orphan-pruning logic
are described in §3.1.

### 3.7 Monitoring — `monitoring/`

Prometheus (`monitoring/prometheus.yml`) scrapes HAProxy's exporter on `:8405`
and node metrics; Grafana renders `monitoring/grafana-dashboard.json`. The
registrar also exposes custom counters/gauges at `GET /metrics`
(enrollments, active volunteers, per-host weight/cpu/mem). Use these for a "we
can observe the fleet live" slide.

---

## 4. How the modules connect (redraw the diagram from this)

**Topology.** One **controller** = Proxmox **LXC 102** running a Docker Compose
stack: the **registrar** container (which *also* runs HAProxy in the same
container — they share the admin socket and config file), the **tunnel** server
(host networking, so it can bind the 30000–31000 pool), the **registry**, and
**node-exporter**. Volunteers = **LXC 111/112/113** in the lab (and, in principle,
any Docker host reachable over the VPN). A **build box** = **LXC 200** compiles
images and runs the deploy. `scripts/redeploy.sh` orchestrates build → `docker
save`/`pct push`/`load` → restart.

**Edges and who calls whom:**
- Visitor → **HAProxy :80** (controller). HAProxy → `127.0.0.1:<public_port>`
  (tunnel server, same host).
- Volunteer **tunnel client** → tunnel server **:9007** (control, outbound) and →
  **:<data_port>** (per stream, outbound). Server → client only over the already-
  open control socket (`PING`/`OPEN`). No inbound to the volunteer, ever.
- Volunteer **agent** → registrar **:3000** (`/enroll`, `/heartbeat`). Registrar →
  HAProxy via the **admin socket** (runtime) and **`haproxy.cfg` + SIGUSR2**
  (config). Registrar → **SQLite** (file).
- Volunteer **agent** → Docker **socket** (run service container) → service
  container reachable at its internal Docker IP, which the tunnel client dials.
- Volunteer host Docker → **registry :5000** (image pull).
- Prometheus → HAProxy **:8405** and exporters; Grafana → Prometheus.

**Failure behavior (important for the deck):**
- *Volunteer disconnects / reboots:* heartbeats stop → churn evicts after 45 s →
  removed from HAProxy (no more traffic), SQLite, and memory → its service is
  re-queued to `pending_services`. When it (or any node) re-enrolls, identity is
  reused by hostname and the service is re-attached. Net effect: a brief outage
  for that one site, then self-heal.
- *Service container crashes:* `--restart unless-stopped` brings it back; HAProxy's
  health check (`GET /` → 2xx) marks the backend down meanwhile, so visitors get
  503 for that site only until it recovers.
- *Tunnel session drops:* client reconnects, gets new ports; because the registrar
  re-points on the next enroll/heartbeat path and the public port is now stable
  per node life, routing converges.
- *Controller / registrar restart:* recovery replays SQLite, waits for the HAProxy
  socket, restores backends + servers. HAProxy itself, on a fresh start, reads the
  whole `haproxy.cfg` so all backends load at once.
- *Backend genuinely unreachable* (no healthy `primary`): that service's frontend
  ACL still matches but the backend is empty → HAProxy returns **503** for that
  host (it does *not* fall through to `default_backend`).

---

## 5. End-to-end flows (traced across modules)

### Flow A — A visitor request reaches a hosted site
1. `GET http://myapp.<BASE_DOMAIN>/` arrives at **HAProxy :80** on the controller.
2. HAProxy evaluates `acl host_svc-myapp hdr(host) -i myapp.<BASE_DOMAIN>` →
   `use_backend svc_svc-myapp`.
3. The backend's single `primary` server is `127.0.0.1:<public_port>` — the
   controller-local mouth of that volunteer's tunnel. HAProxy opens a connection
   there.
4. The **tunnel server** accepts the visitor on `<public_port>`, assigns a
   `stream_id`, stashes the visitor socket, and sends `OPEN(stream_id)` down the
   control channel to the **volunteer's tunnel client**.
5. The client dials the server's **data port**, sends `stream_id`, then connects
   to its **local service** (`LOCAL_SERVICE_ADDR = <container_ip>:<port>`, read
   fresh for this stream) and splices.
6. The **service container** (e.g. the capped `svc-pc` nginx) serves the response;
   bytes flow back through the same splice → tunnel → HAProxy → visitor.
   *Observable proof:* `curl -H 'Host: svc-a.<BASE_DOMAIN>' http://<CONTROLLER_IP>/`
   returns that service's page; the load test in §6 exercises exactly this path.

### Flow B — A volunteer joins and starts hosting
1. Volunteer container boots; `entrypoint.sh` starts nginx (placeholder), the
   tunnel client, and the agent.
2. The tunnel client completes the handshake and writes `<public_port>` to
   `/tmp/tunnel_public_port`.
3. The agent reads that port and `POST /enroll`s with its stable `HOSTNAME` (node
   key) + static specs + the public port.
4. The registrar looks up the node key: if known (in memory or SQLite) it **reuses
   the UUID and re-attaches any prior assignment**; otherwise it mints a UUID and
   **dequeues a `pending_service`**. Either way it calls `assign_service`
   (creates/repoints `svc_<name>` to `127.0.0.1:<public_port>`), persists, and
   returns the assignment in the enroll response.
5. The agent runs the assigned image via the Docker socket **with `SERVICE_CPUS`/
   `SERVICE_MEMORY` caps**, reads the container IP, and writes
   `LOCAL_SERVICE_ADDR`. No tunnel restart needed.
6. Heartbeats begin (every 15 s); each recomputes the weight and pushes it to
   HAProxy. The site is now live and load-balanced by capacity.

### Flow C — A volunteer dies mid-hosting, and the site recovers
1. The node stops heartbeating (power off, network loss).
2. Within 45 s, **churn** removes it from memory/SQLite/HAProxy and pushes its
   service back to `pending_services`.
3. Visitors to that site get 503 (empty backend) during the gap.
4. The same node returns (or a different free one enrolls): identity reuse / the
   pending queue re-assigns the service, the agent re-runs the container, HAProxy
   is repointed, and the site is live again — no operator action.

---

## 6. Current state — demonstrable vs. partial (be honest)

**Demonstrable end-to-end (show these):**
- Service registration → assignment → image pull → container run → HAProxy routing
  → live response, across multiple distinct services on multiple volunteers.
- **Stable identity across a full redeploy** — same UUIDs, assignments survive
  (the headline correctness result; it was broken before and is now fixed).
- **Resource-capped node evaluation** with k6: a 0.5-CPU/256-MB node was measured
  at ~4× the p95 latency of an unlimited node yet still well under the 300 ms
  objective with 0 % errors (`experiments/results/report.md`; the all-services run
  uses `experiments/load-test-all.js`). This is the concrete "it works on weak
  hardware, with a measurable cost" result.
- Crash recovery of the controller; churn + re-queue; capacity-aware weighting;
  live Prometheus/Grafana observability.

**Partial / fragile / deferred (don't oversell):**
- **Reloads wipe runtime-added servers.** Registering a *new* service triggers a
  HAProxy reload, and runtime-added servers (which aren't in the file) are lost
  until the registrar re-adds them — currently only guaranteed on a restart. The
  serialized-reload fix prevents *dropped* reloads but not this wipe. **Operational
  rule today: don't register services during a live demo; register, restart once,
  then run.** Proper fix (re-add all servers after any reload, or pin them in the
  file) is open.
- **Remote volunteers over the VPN are not turnkey.** Docker Desktop on
  Windows/WSL doesn't share the host's Tailscale subnet routes, so a laptop
  volunteer couldn't reach the registry/tunnel in testing. The reliable path is a
  node co-located where `<CONTROLLER_IP>` is directly routable. `docs/handoff-
  network-and-agent-rewrite.md` covers the intended WireGuard/port-exposure model.
- **One volunteer per service** (v1). Multi-volunteer-per-domain load balancing,
  re-assignment policies beyond re-queue, registry TLS+auth, image-update/redeploy
  triggers, and wildcard DNS for `*.<BASE_DOMAIN>` are all in `PLAN.md`'s Open
  Questions.
- **Weight doesn't reflect cgroup caps** (metrics read host `/proc`) — cosmetic
  for the dashboard, irrelevant to the measured service performance.
- **Trust model is wide open** (host Docker socket + controller-supplied images) —
  fine for a single operator, must change before third-party enrollment.
- A Rust rewrite of the volunteer agent (single static binary, `FROM scratch`,
  ~15–20 MB) is designed but not built — `docs/handoff-network-and-agent-rewrite.md`.

---

## 7. Suggested skills / tools for the next agent (the deck builder)

- **`pptx`** (python-pptx) to generate the `.pptx` directly. Recommended slide
  spine, mapping to this doc:
  1. Title / authors / institution — from `README.md`.
  2. Problem & motivation — §1 (lead with the non-profit, anti-oligopoly framing;
     contrast with cloud and crypto-DePIN).
  3. Where we came from (TCC1) — cite `docs/tcc1/*.pdf`.
  4. 30-second mental model — §2 (one diagram).
  5. Architecture diagram — redraw from §4 (controller box with HAProxy/registrar/
     tunnel/registry; volunteer boxes; arrows labeled with protocols/ports).
  6. Reverse tunnel deep-dive — §3.3 + Flow A (this is the technically novel bit;
     spend time here).
  7. Lifecycle & resilience — Flows B and C (enroll/assign, die/recover).
  8. Capacity-aware load balancing + stable identity — the two best
     engineering-decision slides (§3.1).
  9. Evaluation/results — the k6 table from `experiments/results/report.md`
     (latency vs 300 ms objective, 0 % errors, capped vs uncapped).
  10. Current state & limitations — §6, honestly.
  11. Future work — `PLAN.md` open questions + `docs/handoff-network-and-agent-
      rewrite.md`.
- **`mermaid`/diagram tooling** for the architecture and sequence diagrams (§4/§5
  are written to be transcribed almost verbatim into a sequence diagram).
- For live-demo backup, capture screenshots of the registrar dashboard (`GET /`)
  and Grafana ahead of time in case the network misbehaves during the defense.

**Do not** re-derive numbers — pull them from `experiments/results/report.md` /
`report-all.md`. **Do** read `PLAN.md` before writing the "decisions" and "future
work" slides; it is the canonical record of what was chosen and what was punted.
