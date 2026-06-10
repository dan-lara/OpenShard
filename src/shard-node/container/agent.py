#!/usr/bin/env python3
"""
OpenShard Volunteer Agent
Handles enrollment and heartbeat with the registrar.

Startup sequence:
  1. Rust client connects to tunnel server, negotiates data port + public port.
  2. Rust client writes the assigned public port to /tmp/tunnel_public_port.
  3. This agent reads that port and includes it in the /enroll payload.
  4. Heartbeat loop runs until the process exits.
"""

import os
import subprocess
import time
import json
import socket
import platform
import urllib.request
import urllib.error
import shutil


# ── Config from environment ───────────────────────────────────────────────────
REGISTRAR_URL       = os.environ.get("REGISTRAR_URL", "http://host.docker.internal:3000")
SERVICE_PORT        = int(os.environ.get("SERVICE_PORT", "8080"))
HEARTBEAT_INTERVAL  = int(os.environ.get("HEARTBEAT_INTERVAL", "15"))
HOSTNAME            = os.environ.get("HOSTNAME", socket.gethostname())
TUNNEL_VERSION      = "0.1.0"

# File the Rust client writes after completing the tunnel handshake.
TUNNEL_PUBLIC_PORT_FILE = "/tmp/tunnel_public_port"
# File this agent writes so the Rust client knows which data port to request.
TUNNEL_DATA_PORT_FILE   = "/tmp/tunnel_data_port"
# File this agent writes so the Rust client knows the local service address.
LOCAL_SERVICE_ADDR_FILE = "/tmp/tunnel_local_addr"

# Set after enrollment; used by run_service() to name the service container.
VOLUNTEER_ID: str = ""


# ── Metrics ───────────────────────────────────────────────────────────────────
def read_cpu_pct() -> float:
    def read_stat():
        with open("/proc/stat") as f:
            line = f.readline()
        fields = list(map(int, line.split()[1:]))
        idle = fields[3]
        total = sum(fields)
        return idle, total

    idle1, total1 = read_stat()
    time.sleep(0.5)
    idle2, total2 = read_stat()

    idle_delta  = idle2  - idle1
    total_delta = total2 - total1
    if total_delta == 0:
        return 0.0
    return round((1.0 - idle_delta / total_delta) * 100.0, 2)


def read_mem_pct() -> float:
    info = {}
    with open("/proc/meminfo") as f:
        for line in f:
            key, val = line.split(":")
            info[key.strip()] = int(val.strip().split()[0])
    total     = info.get("MemTotal", 1)
    available = info.get("MemAvailable", 0)
    return round(((total - available) / total) * 100.0, 2)


def read_load_avg() -> float:
    with open("/proc/loadavg") as f:
        return float(f.read().split()[0])


def get_cpu_model() -> str:
    try:
        with open("/proc/cpuinfo") as f:
            for line in f:
                if line.startswith("model name"):
                    return line.split(":", 1)[1].strip()
    except Exception:
        pass
    return "unknown"


def get_docker_version() -> str:
    # Try docker socket via env var passed from host, or docker CLI if present
    version = os.environ.get("DOCKER_VERSION")
    if version:
        return version
    if shutil.which("docker"):
        try:
            result = subprocess.run(
                ["docker", "--version"],
                capture_output=True, text=True, timeout=2
            )
            return result.stdout.strip().split()[2].rstrip(",")
        except Exception:
            pass
    return "unknown"


def _total_mem_mb() -> int:
    try:
        with open("/proc/meminfo") as f:
            for line in f:
                if line.startswith("MemTotal"):
                    return int(line.split()[1]) // 1024
    except Exception:
        pass
    return 0


def _disk_free_gb() -> int:
    try:
        st = os.statvfs("/")
        return (st.f_bavail * st.f_frsize) // (1024 ** 3)
    except Exception:
        return 0


# ── Service lifecycle ─────────────────────────────────────────────────────────

def run_service(image: str, service_port: int) -> str:
    """Pull and run service container (no published ports). Idempotent. Returns container IP."""
    name = f"svc_{VOLUNTEER_ID}"
    subprocess.run(["docker", "rm", "-f", name],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["docker", "pull", image], check=True)
    subprocess.run([
        "docker", "run", "-d", "--restart", "unless-stopped", "--name", name, image
    ], check=True)
    ip = subprocess.run(
        ["docker", "inspect", "-f",
         "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}", name],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    return ip


def _restart_tunnel_client() -> int:
    """Kill current tunnel client, start a fresh one, and return the new public port."""
    try:
        os.remove(TUNNEL_PUBLIC_PORT_FILE)
    except FileNotFoundError:
        pass
    subprocess.run(["pkill", "-f", "/app/client"],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    subprocess.Popen(["/app/client"])
    return wait_for_tunnel_public_port()


def apply_service_assignment(image: str, service_port: int) -> int:
    """Run service container, update LOCAL_SERVICE_ADDR, restart tunnel. Returns new public port."""
    print(f"[agent] Starting service image={image} port={service_port}")
    ip = run_service(image, service_port)
    addr = f"{ip}:{service_port}"
    print(f"[agent] Service container up at {addr}")
    with open(LOCAL_SERVICE_ADDR_FILE, "w") as f:
        f.write(addr)
    print(f"[agent] LOCAL_SERVICE_ADDR set to {addr}")
    return _restart_tunnel_client()


# ── HTTP helpers ──────────────────────────────────────────────────────────────

# Module-level so post() can update it and last_status_code() can read it.
_last_status_code = 0

def last_status_code() -> int:
    return _last_status_code

def post(path: str, payload: dict) -> dict | None:
    global _last_status_code
    url  = f"{REGISTRAR_URL}{path}"
    data = json.dumps(payload).encode()
    req  = urllib.request.Request(
        url, data=data,
        headers={"Content-Type": "application/json"},
        method="POST"
    )
    print(f"[agent] Outgoing HTTP POST to: {url}")
    print(f"[agent] Outgoing HTTP Payload: {json.dumps(payload)}")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            _last_status_code = resp.status
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        _last_status_code = e.code
        print(f"[agent] HTTP error {e.code} on {path}: {e.read().decode()}")
    except urllib.error.URLError as e:
        print(f"[agent] Connection error on {path}: {e.reason}")
    except Exception as e:
        print(f"[agent] Unexpected error on {path}: {e}")
    return None


# ── Tunnel handshake wait ─────────────────────────────────────────────────────

def wait_for_tunnel_public_port() -> int:
    """
    Block until the Rust client writes the assigned public port to
    /tmp/tunnel_public_port, then return it as an int.
    """
    print(f"[agent] Waiting for Rust client to complete tunnel handshake…")
    backoff = 1
    while True:
        try:
            content = open(TUNNEL_PUBLIC_PORT_FILE).read().strip()
            port = int(content)
            print(f"[agent] Tunnel handshake complete — public port: {port}")
            return port
        except (FileNotFoundError, ValueError):
            time.sleep(backoff)
            backoff = min(backoff * 2, 10)


# ── Enrollment ────────────────────────────────────────────────────────────────

def enroll(tunnel_public_port: int) -> str | None:
    """Register with the registrar. Returns volunteer_id or None on failure."""
    payload = {
        "os":                platform.system() + " " + platform.release(),
        "arch":              platform.machine(),
        "cpu_cores":         os.cpu_count() or 1,
        "cpu_model":         get_cpu_model(),
        "memory_total_mb":   _total_mem_mb(),
        "disk_free_gb":      _disk_free_gb(),
        "docker_version":    get_docker_version(),
        "tunnel_version":    TUNNEL_VERSION,
        "service_addr":      f"localhost:{SERVICE_PORT}",
        "hostname":          HOSTNAME,
        "tunnel_public_port": tunnel_public_port,
    }

    print(f"[agent] Enrolling with tunnel_public_port={tunnel_public_port}")
    resp = post("/enroll", payload)

    if resp and "volunteer_id" in resp:
        vid = resp["volunteer_id"]
        print(f"[agent] Enrolled successfully — volunteer_id={vid}")
        return vid

    print("[agent] Enrollment failed.")
    return None


# ── Heartbeat loop ────────────────────────────────────────────────────────────

def heartbeat_loop(vid: str, tunnel_public_port: int):
    current_vid = vid

    while True:
        time.sleep(HEARTBEAT_INTERVAL)

        try:
            cpu  = read_cpu_pct()
            mem  = read_mem_pct()
            load = read_load_avg()
        except Exception as e:
            print(f"[agent] Failed to read metrics: {e}")
            cpu, mem, load = 0.0, 0.0, 0.0

        payload = {
            "volunteer_id":    current_vid,
            "cpu_pct":         cpu,
            "mem_pct":         mem,
            "load_avg":        load,
            "active_requests": 0,
        }

        resp = post("/heartbeat", payload)

        if resp:
            weight = round(100 - (cpu * 0.6 + mem * 0.4), 1)
            print(f"[agent] Heartbeat sent — cpu={cpu}% mem={mem}% weight={weight}")

        elif last_status_code() == 404:
            # Registrar lost state (e.g. restarted) — re-enroll.
            print(f"[agent] Registrar doesn't recognise volunteer_id={current_vid} — re-enrolling…")
            backoff = 2
            new_vid = None
            while new_vid is None:
                # Re-read the public port in case the tunnel reconnected.
                try:
                    tunnel_public_port = int(open(TUNNEL_PUBLIC_PORT_FILE).read().strip())
                except (FileNotFoundError, ValueError):
                    pass
                new_vid = enroll(tunnel_public_port)
                if new_vid is None:
                    print(f"[agent] Re-enrollment failed, retrying in {backoff}s…")
                    time.sleep(backoff)
                    backoff = min(backoff * 2, 30)
            current_vid = new_vid
            print(f"[agent] Re-enrolled — new volunteer_id={current_vid}")

        else:
            print("[agent] Heartbeat failed — registrar unreachable, will retry")


# ── Main ──────────────────────────────────────────────────────────────────────

def main():
    print("[agent] OpenShard Volunteer Agent starting")
    print(f"[agent] Registrar: {REGISTRAR_URL}")
    print(f"[agent] Service:   :{SERVICE_PORT}")

    # Wait for the Rust client to finish the tunnel handshake and tell us
    # which public port the tunnel server assigned.
    tunnel_public_port = wait_for_tunnel_public_port()

    # Enroll with the registrar, retrying with backoff on failure.
    backoff = 2
    vid = None
    while vid is None:
        vid = enroll(tunnel_public_port)
        if vid is None:
            print(f"[agent] Retrying enrollment in {backoff}s…")
            time.sleep(backoff)
            backoff = min(backoff * 2, 30)

    global VOLUNTEER_ID
    VOLUNTEER_ID = vid

    heartbeat_loop(vid, tunnel_public_port)


if __name__ == "__main__":
    main()