# Handoff: Network Exposure & Volunteer Agent Rewrite

Two deferred decisions captured here for future implementation.

---

## 1. Network Exposure — What Should Be Public vs Private

### Decision

Only port 80 (HAProxy, public traffic) should be reachable from the internet.
Everything else is internal between the controller and its workers.

### Port map

| Port | Protocol | Bind | Who reaches it | Notes |
|------|----------|------|----------------|-------|
| 80 | HTTP | `0.0.0.0` | Internet → HAProxy | Public — serves routed domains |
| 3000 | HTTP | `10.10.10.10` | Workers only | Registrar API (enroll / heartbeat / services) |
| 5000 | HTTP | `10.10.10.10` | Workers only | Docker image registry (plain HTTP) |
| 8405 | HTTP | `10.10.10.10` | Monitoring only | HAProxy Prometheus metrics scrape |
| 9007 | TCP | all (firewall) | Workers only | Tunnel control port |
| 30000–31000 | TCP | all (firewall) | HAProxy on same host | Tunnel public ports — never leave the machine |

### Implementation

**`docker-compose.yml` — bind backoffice ports to internal interface only:**

```yaml
registrar:
  ports:
    - "0.0.0.0:80:80"
    - "10.10.10.10:3000:3000"
    - "10.10.10.10:8405:8405"

registry:
  ports:
    - "10.10.10.10:5000:5000"
```

**Tunnel server (ports 9007 + 30000–31000):** runs `network_mode: host` so it binds to all
interfaces today. Two paths to fix this:

- **Short term**: Proxmox host firewall blocks those ranges on the public interface.
  Workers already use `10.10.10.10` so nothing changes on their side.
- **Long term**: pass `BIND_ADDR=10.10.10.10` env var to `server.rs` and make it
  bind only there (one-line change in `const` or `env::var`).

**Worker Docker daemon** — required on every worker LXC to allow plain-HTTP pulls from the
internal registry:

```json
// /etc/docker/daemon.json on each worker LXC
{ "insecure-registries": ["10.10.10.10:5000"] }
```

Then `systemctl restart docker` on each worker.

### Remote workers (future)

If workers ever run outside the Proxmox host (different datacenters, user-donated machines),
add WireGuard between them and the controller. The internal IP stays the same from the
workers' perspective — only the routing changes. `REGISTRAR_URL`, `TUNNEL_SERVER`, and the
registry address all remain `10.10.10.10`.

---

## 2. Volunteer Agent Rewrite — Rust

### Current state (post-optimisation)

After switching from `python:3.12-alpine` to `alpine:3.21` + `python3`, and removing
`docker-cli` in favour of direct Docker socket HTTP calls from stdlib, the volunteer image
is **74 MB**.

Remaining breakdown:
```
~9 MB   Alpine base
~15 MB  python3
~5 MB   nginx (default placeholder page)
~1 MB   Rust client binary (musl static)
```

### Target state (Rust rewrite)

Replace `agent.py` + `python3` + `entrypoint.sh` + nginx with a **single statically compiled
musl Rust binary** that does everything:

- Tunnel client (existing `client.rs` logic, moved to a thread)
- Enroll / heartbeat loop (existing `agent.py` logic)
- Docker socket management (same HTTP-over-Unix-socket, now in Rust)
- `/proc` metrics reading (trivial file I/O, no crate needed)
- Built-in minimal HTTP server (returns 503 when no service assigned — replaces nginx)

Use `FROM scratch` for the final image. Expected result: **~15–20 MB**.

### Architecture of the combined binary

```
main()
  ├── spawn tunnel_thread   (connects to TUNNEL_SERVER:9007, maintains session)
  ├── spawn agent_thread    (enroll, heartbeat every 15s, docker management)
  └── spawn http_thread     (binds SERVICE_PORT, returns 503 / health-check OK)

tunnel_thread ←──channel──→ agent_thread
  public_port TX              public_port RX (used in enroll payload)
  local_addr  RX              local_addr  TX (set after service assignment)
```

Channels replace the current file-based IPC (`/tmp/tunnel_public_port`,
`/tmp/tunnel_local_addr`).

### Crate dependencies (keep minimal)

| Need | Crate | Why |
|------|-------|-----|
| HTTP to registrar | `ureq` | sync, small (~300 KB compiled), no tokio |
| Docker socket | none | raw `UnixStream` + manual HTTP framing (already proved in Python) |
| JSON | `serde_json` | already in workspace |
| Everything else | std | file I/O, threads, channels, TCP |

Avoid `reqwest` / `tokio` — they pull in async runtimes and significantly inflate binary size.

### Why deferred

The current service-assignment feature (tasks 2–14) is being validated first.
The rewrite is straightforward (~200–300 lines of Rust) but introduces risk before the
feature is confirmed working end-to-end. Revisit once:

1. Service registration → assignment → tunnel → HAProxy routing is confirmed working.
2. The `insecure-registries` + internal registry pull is confirmed working on workers.
3. Two volunteers on one host (LXC 113 proof-of-concept) is confirmed working.
