# Plan: Service Assignment via Central Registry (Option C) — Loop-Ready

## Goal

Enable transparent service assignment to volunteer nodes. A service owner pushes a Docker image
to a central registry on the controller and registers the service. The registrar assigns that image
to an available volunteer. The volunteer pulls and runs it automatically — no manual configuration
on the volunteer side.

---

## Canonical names — CONFIRM before launching the loop

These values get written verbatim into HAProxy ACLs, DNS, and env vars. A mismatch fails silently
(health checks DOWN, 404s). Set them ONCE here and the agent must reuse them everywhere.

- **Base domain:** `openshard.danlara.com.br`
  - ⚠️ Your live system is `openshrd.danlara.com.br` (no `a`). Pick one. It must match DNS and the
    HAProxy `hdr(host)` ACLs character-for-character. The whole plan uses `openshard` as a placeholder.
- **Registry (v1, internal-only):** `10.10.10.10:5000` — added to `insecure-registries` on the
  controller **and every volunteer host's** Docker daemon. TLS + the `registry.<domain>` name is a
  later upgrade, not part of this loop.
- **Controller internal IP:** `10.10.10.10`
- **Volunteer image:** `openshard/volunteer:latest`
- **Tunnel server host (for volunteers):** `openshard.danlara.com.br`

---

## Committed design decisions (forks resolved)

1. **No published ports on service containers.** The agent runs the assigned image with
   `docker run -d` (no `-p`) and reads the container's internal Docker network IP via `docker inspect`.
   This is the only design in this plan — the old `-p 8080:8080` / fixed-port approach is removed
   because it collides when multiple volunteer containers share one host.
2. **Services declare their listening port at registration.** No 8080 assumption. `port` is part of
   `RegisterServiceRequest` and flows through to the agent.
3. **`LOCAL_SERVICE_ADDR` becomes configurable.** `client.rs` reads it from env/file instead of the
   hardcoded `127.0.0.1`. The agent sets it to `<container_ip>:<service_port>` after the service is
   running, then (re)starts the tunnel client.
4. **HAProxy backend target = `127.0.0.1:<tunnel_public_port>`** (the controller-local listen port of
   the reverse tunnel for the assigned volunteer). ⚠️ If HAProxy runs in its **own** container rather
   than on the controller host, replace `127.0.0.1` with the host gateway `172.17.0.1`. CONFIRM CT layout.
5. **Tunnel port reuse.** The registrar already allocates a per-volunteer `tunnel_public_port` at
   enroll. Reuse it as the HAProxy backend target — do **not** build a new allocator. CONFIRM the
   existing field name in the enroll flow.
6. **Assignment happens at enroll time only** for v1. Dynamic re-assignment / assigning to
   already-enrolled idle volunteers is deferred (see Open Questions).

---

## Verification per task (used by the loop)

- Registrar / `client.rs` tasks (Rust): `cargo check` must pass (and `cargo clippy` if available).
- Volunteer tasks (`agent.py`, volunteer `docker run`/Dockerfile): `docker build` of
  `openshard/volunteer` must succeed.
- Infra / compose tasks: `docker compose config` must validate.
- **Do not check a box unless its verifier passes.** Most tasks touch only one side — run the
  verifier that applies, not all three.

---

## Data path (target)

```
Internet
  └─ curl http://myapp.openshard.danlara.com.br
        │
        ▼
  HAProxy (controller, :80)
  ├─ acl host_myapp hdr(host) -i myapp.openshard.danlara.com.br
  └─ use_backend svc_myapp  →  127.0.0.1:30XXX   (tunnel_public_port of assigned volunteer)
        │  reverse tunnel
        ▼
  Volunteer tunnel client (client.rs)
  └─ LOCAL_SERVICE_ADDR = 172.18.0.5:<service_port>   (set by agent after run)
        │
        ▼
  Service container (no published ports), listening on <service_port>
```

---

## Components

### Docker Registry (controller)

Add to the controller `docker-compose.yml`:
```yaml
registry:
  image: registry:2
  restart: unless-stopped
  ports:
    - "5000:5000"
  volumes:
    - registry-data:/var/lib/registry
```
v1 is plain HTTP at `10.10.10.10:5000`. Add `{"insecure-registries": ["10.10.10.10:5000"]}` to
`/etc/docker/daemon.json` on the controller and **every volunteer host**, then restart dockerd.

Owners push:
```bash
docker tag myapp 10.10.10.10:5000/myapp:latest
docker push 10.10.10.10:5000/myapp:latest
```

### Registrar (`state.rs`, `services.rs`, `enrollment.rs`, `db.rs`, `haproxy_manager`)

`VolunteerState` gains `assigned_service: Option<ServiceAssignment>`.
`AppState` gains `pending_services: Arc<RwLock<VecDeque<PendingService>>>`.

```rust
pub struct ServiceAssignment {
    pub service_name: String,
    pub domain: String,
    pub image: String,
    pub service_port: u16,
    pub assigned_at: chrono::DateTime<chrono::Utc>,
}

pub struct PendingService {
    pub name: String,
    pub domain: String,
    pub image: String,
    pub service_port: u16,
    pub registered_at: chrono::DateTime<chrono::Utc>,
}

pub struct RegisterServiceRequest {
    pub name: String,
    pub domain: String,
    pub image: String,      // e.g. 10.10.10.10:5000/myapp:latest
    pub port: u16,          // port the app listens on INSIDE its container
}

pub struct EnrollResponse {
    pub volunteer_id: String,
    pub heartbeat_interval_seconds: u64,
    pub assignment: Option<AssignmentPayload>,
}

pub struct AssignmentPayload {
    pub image: String,
    pub service_port: u16,
}
```

`haproxy_manager::assign_service(domain, volunteer_id, tunnel_addr)` (runtime API, already wired):
- create backend `svc_<name>` with `server vol-<volunteer_id> <tunnel_addr> check`
  where `tunnel_addr = "127.0.0.1:<tunnel_public_port>"`
- `acl host_<name> hdr(host) -i <domain>`
- `use_backend svc_<name> if host_<name>`

`db.rs` adds:
```sql
CREATE TABLE IF NOT EXISTS service_assignments (
    volunteer_id  TEXT PRIMARY KEY,
    service_name  TEXT NOT NULL,
    domain        TEXT NOT NULL,
    image         TEXT NOT NULL,
    service_port  INTEGER NOT NULL,
    assigned_at   TEXT NOT NULL
);
```

### Volunteer Agent (`agent.py`)

```python
def run_service(image: str, service_port: int) -> str:
    name = f"svc_{VOLUNTEER_ID}"
    subprocess.run(["docker", "rm", "-f", name],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)  # idempotent
    subprocess.run(["docker", "pull", image], check=True)
    subprocess.run([
        "docker", "run", "-d", "--restart", "unless-stopped", "--name", name, image
    ], check=True)                                                       # no -p
    ip = subprocess.run([
        "docker", "inspect", "-f",
        "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}", name
    ], capture_output=True, text=True, check=True).stdout.strip()
    return ip  # e.g. 172.18.0.5
```
After `run_service`, the agent writes `LOCAL_SERVICE_ADDR=<ip>:<service_port>` to the file/env the
tunnel client reads (same channel as `tunnel_public_port` today) and (re)starts the tunnel client so
it reverse-tunnels `<ip>:<service_port>`.

Volunteer `docker run` (host side):
```bash
docker run -d \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -e TUNNEL_SERVER=openshard.danlara.com.br \
  -e REGISTRAR_URL=http://openshard.danlara.com.br:3000 \
  openshard/volunteer:latest
```

### HAProxy (managed dynamically via runtime API)

```
frontend http-in
  bind *:80
  acl host_myapp hdr(host) -i myapp.openshard.danlara.com.br
  use_backend svc_myapp if host_myapp
  default_backend volunteers          # unassigned traffic

backend svc_myapp
  server vol-<uuid> 127.0.0.1:30000 check
```

---

## Implementation Order

Ordered so every task compiles / builds in isolation: types and functions are created before the
code that references them. The verifier in parentheses is what must pass before checking the box.

- [x] 1. Add the `registry:2` service to controller `docker-compose.yml` and document the
      `insecure-registries` daemon config for controller + volunteer hosts. (docker compose config)
- [x] 2. `state.rs`: add `ServiceAssignment` and `PendingService` structs, `assigned_service` on
      `VolunteerState`, and `pending_services` queue on `AppState`. (cargo check)
- [x] 3. `enrollment.rs` (types only): define `AssignmentPayload`, add
      `assignment: Option<AssignmentPayload>` to `EnrollResponse`, and update **every existing
      `EnrollResponse` construction site** to pass `assignment: None`. (cargo check)
- [x] 4. `db.rs`: add the `service_assignments` table and insert/load helpers. (cargo check)
- [x] 5. `haproxy_manager::assign_service(domain, volunteer_id, tunnel_addr)` — create per-service
      backend, ACL, and `use_backend` via the runtime API. (cargo check)
- [x] 6. `services.rs`: add `image` + `port` to `RegisterServiceRequest`; on register, assign to a free
      volunteer immediately (call `assign_service`) else push to `pending_services`. (cargo check)
- [x] 7. `enrollment.rs` (logic): on enroll, dequeue a `PendingService`, call `assign_service` with
      `127.0.0.1:<tunnel_public_port>`, persist via `db.rs`, set `assigned_service`, and populate
      `assignment` in the response. (cargo check)
- [x] 8. Registrar: accept `service_running` / `service_image` fields in the heartbeat handler and
      store them on `VolunteerState`. (cargo check)
- [x] 9. Dashboard: show the assigned service (name + image + running status) per volunteer. (cargo check)
- [x] 10. `client.rs`: read `LOCAL_SERVICE_ADDR` from env/file at startup instead of the hardcoded
      `127.0.0.1`; support a restart/reload so a new address takes effect. (cargo check)
- [x] 11. `agent.py`: implement `run_service(image, service_port)` (idempotent, no published ports,
      returns container IP); set `LOCAL_SERVICE_ADDR` and (re)start the tunnel client. (docker build)
- [x] 12. `agent.py`: handle `assignment` in the enroll response → call `run_service`. (docker build)
- [x] 13. `agent.py`: track the service container id; report `service_running` / `service_image` on
      heartbeat. (docker build)
- [ ] 14. Update the volunteer `docker run` / Dockerfile to mount `/var/run/docker.sock` and set
      `TUNNEL_SERVER` + `REGISTRAR_URL` to the canonical domain. (docker build)

---

## Scaling model

Each volunteer container handles exactly one service. To host more services on one physical host,
run more volunteer containers — each enrolls and gets one pending service assigned. Because service
containers publish no ports and are reached by their internal Docker IP, multiple volunteer
containers coexist on one Docker daemon without port conflicts.

```
Physical host
├── volunteer #1 → tunnel:30000 → svc_<id1> (172.18.0.5:PORT)  myapp
├── volunteer #2 → tunnel:30002 → svc_<id2> (172.18.0.6:PORT)  blog
└── volunteer #3 → tunnel:30004 → svc_<id3> (172.18.0.7:PORT)  api
```

---

## Open Questions (deferred — NOT in this loop)

- **Re-assignment on disconnect:** when a volunteer drops, reassign its service to another volunteer.
- **Multi-volunteer per service:** N volunteers load-balanced behind one domain.
- **Registry auth + TLS:** v1 is internal-only `insecure-registries`; production needs TLS and creds.
- **Image updates / redeploy:** how an owner pushes a new image and triggers a pull+restart.
- **Assign to already-enrolled idle volunteers:** v1 only assigns at enroll time.

Resolved here: port conflict (→ internal Docker IP), `LOCAL_SERVICE_ADDR` hardcode (→ tasks 10–11),
crash restart (→ `--restart unless-stopped` + heartbeat reporting), per-service routing target
(→ `127.0.0.1:<tunnel_public_port>`).

---

## Trust assumption (explicit)

The volunteer container mounts `/var/run/docker.sock` and runs controller-supplied images. This gives
the controller root-equivalent control of every volunteer host. Acceptable for a single-operator lab;
revisit before opening enrollment to untrusted third parties.
