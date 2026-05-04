# Tunnel Experiment

A working proof-of-concept of the OpenShard reverse tunnel. It demonstrates how a **main server** (publicly accessible) can route HTTP traffic to **volunteer agents** running behind NATs, without ever exposing the volunteers' networks directly.

See [`arch.md`](arch.md) for architecture diagrams and a detailed explanation of how the pieces fit together.

---

## How It Works

```
Internet → main-server :8080 (HTTP) ──► gRPC stream ──► volunteer-agent ──► target service :9000
                        :50051 (gRPC/TLS, for agents)
```

1. Each volunteer agent opens an outbound TLS/gRPC bidirectional stream to the main server and sends a `REGISTER` frame.
2. Every 10 s the agent sends a `HEARTBEAT` with CPU, memory, and load metrics.
3. When an HTTP request hits port `8080`, the main server picks the healthiest registered agent, serializes the request into a `TunnelFrame` (protobuf), and sends it down that stream.
4. The agent forwards the request to its local target service (`http://127.0.0.1:9000`) and sends the response back up the stream.
5. The main server unblocks the waiting HTTP handler and returns the response to the original client.

---

## Prerequisites

| Tool | Purpose |
|---|---|
| Rust + Cargo | Build `main-server` and `volunteer-agent` |
| OpenSSL | Generate TLS certificates |
| Python 3 | Run the mock target service |

`protoc` is **not** required — a precompiled binary is bundled via `protoc-bin-vendored` and used automatically at build time.

---

## Running on Windows

### 1. Install prerequisites

```powershell
# Rust (if not already installed)
winget install Rustlang.Rustup

# Python 3
winget install Python.Python.3
```

OpenSSL is already available inside **Git for Windows** (Git Bash). No separate install needed for cert generation.

### 2. Generate TLS certificates (one-time, run in Git Bash)

```bash
cd experiments/tunnel/certs
bash gen.sh
```

This produces `ca.crt`, `ca.key`, `server.crt`, and `server.key` inside `certs/`.

### 3. Run everything (PowerShell)

```powershell
cd experiments\tunnel
powershell -ExecutionPolicy Bypass -File start_mesh.ps1
```

Or from Git Bash:

```bash
cd experiments/tunnel
bash start_mesh.sh
```

This starts:
- **Python HTTP server** on `:9000` — mock target service serving local files
- **main-server** on `:50051` (gRPC/TLS) and `:8080` (HTTP dispatch)
- **Two volunteer agents** connecting to `:50051`

### 4. Send a request through the tunnel

```powershell
curl http://127.0.0.1:8080/
start http://127.0.0.1:8080/dashboard
```

### 5. Follow logs

```powershell
Get-Content main.log -Wait   # server heartbeats, registrations, dispatch
Get-Content vol1.log -Wait   # agent 1 activity
```

---

## Manual Start

If you prefer to start each component in separate terminals (run all from `experiments\tunnel\`):

```powershell
# Terminal 1 — mock target service
python -m http.server 9000

# Terminal 2 — main server (reads certs/server.crt and certs/server.key)
cargo run --bin main-server

# Terminal 3 — volunteer agent (reads certs/ca.crt)
cargo run --bin volunteer-agent

# Terminal 4 — second agent (optional, demonstrates load balancing)
cargo run --bin volunteer-agent
```

> **Working directory matters.** Both binaries load TLS certs via relative paths (`certs/server.crt`, `certs/ca.crt`). Always run them from `experiments/tunnel/`.

---

## Ports

| Port | Protocol | Component | Purpose |
|------|----------|-----------|---------|
| 8080 | HTTP | main-server | Receives client requests; dispatches to agents |
| 50051 | gRPC/TLS | main-server | Accepts persistent tunnel streams from agents |
| 9000 | HTTP | target service | Local app served by the volunteer (mock: Python) |

---

## Project Structure

```
experiments/tunnel/
├── certs/
│   ├── cert.conf       # OpenSSL config for CA and CSR generation
│   ├── cert.ext        # Extensions applied when signing the server cert
│   └── gen.sh          # Generates ca.crt, server.crt, server.key
├── main-server/        # Central server: gRPC tunnel broker + HTTP dispatcher
├── tunnel-proto/       # Protobuf schema (TunnelFrame, payloads)
├── volunteer-agent/    # Agent: connects to main server, proxies to local service
├── arch.md             # Architecture diagrams (Mermaid)
├── start_mesh.sh       # One-command launcher (Linux/macOS/Git Bash)
└── start_mesh.ps1      # One-command launcher (Windows PowerShell)
```
