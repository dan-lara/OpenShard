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
import urllib.parse
import http.client


# ── Config from environment ───────────────────────────────────────────────────
REGISTRAR_URL       = os.environ.get("REGISTRAR_URL", "http://host.docker.internal:3000")
SERVICE_PORT        = int(os.environ.get("SERVICE_PORT", "8080"))
HEARTBEAT_INTERVAL  = int(os.environ.get("HEARTBEAT_INTERVAL", "15"))
HOSTNAME            = os.environ.get("HOSTNAME", socket.gethostname())
TUNNEL_VERSION      = "0.1.0"

# Optional resource caps applied to the spawned service container, so a
# constrained donor host can be simulated realistically. Examples:
#   SERVICE_CPUS=0.5      (half a core)
#   SERVICE_MEMORY=256m   (256 MiB hard cap; swap disabled)
SERVICE_CPUS        = os.environ.get("SERVICE_CPUS", "").strip()
SERVICE_MEMORY      = os.environ.get("SERVICE_MEMORY", "").strip()


def _parse_mem_bytes(s: str) -> int:
    """Parse a docker-style memory string (e.g. '256m', '1g', '512k') to bytes."""
    s = s.strip().lower()
    mult = 1
    if s.endswith("g"):
        mult, s = 1024 ** 3, s[:-1]
    elif s.endswith("m"):
        mult, s = 1024 ** 2, s[:-1]
    elif s.endswith("k"):
        mult, s = 1024, s[:-1]
    elif s.endswith("b"):
        s = s[:-1]
    return int(float(s) * mult)


def _service_host_config() -> dict:
    """HostConfig for the service container: restart policy + optional CPU/RAM caps."""
    hc = {"RestartPolicy": {"Name": "unless-stopped"}}
    if SERVICE_CPUS:
        try:
            hc["NanoCpus"] = int(float(SERVICE_CPUS) * 1_000_000_000)
        except ValueError:
            print(f"[agent] invalid SERVICE_CPUS={SERVICE_CPUS!r}, ignoring")
    if SERVICE_MEMORY:
        try:
            mem = _parse_mem_bytes(SERVICE_MEMORY)
            hc["Memory"] = mem
            hc["MemorySwap"] = mem  # equal to Memory → disable swap, hard cap
        except ValueError:
            print(f"[agent] invalid SERVICE_MEMORY={SERVICE_MEMORY!r}, ignoring")
    return hc

# File the Rust client writes after completing the tunnel handshake.
TUNNEL_PUBLIC_PORT_FILE = "/tmp/tunnel_public_port"
# File this agent writes so the Rust client knows which data port to request.
TUNNEL_DATA_PORT_FILE   = "/tmp/tunnel_data_port"
# File this agent writes so the Rust client knows the local service address.
LOCAL_SERVICE_ADDR_FILE = "/tmp/tunnel_local_addr"

# Set after enrollment; used by run_service() to name the service container.
VOLUNTEER_ID: str = ""

# Tracked after run_service(); reported in heartbeat.
_service_container_id: str = ""
_service_image: str = ""


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


# ── Docker socket API (no docker-cli needed) ──────────────────────────────────

class _DockerHTTP(http.client.HTTPConnection):
    """HTTPConnection that talks to the Docker Unix socket."""
    def __init__(self, timeout: float = 30):
        super().__init__("localhost", timeout=timeout)

    def connect(self):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(self.timeout)
        s.connect("/var/run/docker.sock")
        self.sock = s


def _docker(method: str, path: str, body=None, timeout: float = 30):
    conn = _DockerHTTP(timeout=timeout)
    data = json.dumps(body).encode() if body is not None else None
    hdrs = {"Content-Type": "application/json"} if body is not None else {}
    conn.request(method, path, body=data, headers=hdrs)
    resp = conn.getresponse()
    raw = resp.read()
    try:
        result = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError:
        result = {}
    return resp.status, result


def get_docker_version() -> str:
    version = os.environ.get("DOCKER_VERSION")
    if version:
        return version
    try:
        status, info = _docker("GET", "/version")
        if status == 200:
            return info.get("Version", "unknown")
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
    global _service_container_id, _service_image
    name = f"svc_{VOLUNTEER_ID}"

    _docker("DELETE", f"/containers/{urllib.parse.quote(name, safe='')}?force=true")

    print(f"[agent] Pulling {image}…")
    status, _ = _docker("POST", f"/images/create?fromImage={urllib.parse.quote(image, safe='/:@.')}", timeout=300)
    if status != 200:
        raise RuntimeError(f"docker pull {image} failed: HTTP {status}")

    host_config = _service_host_config()
    if SERVICE_CPUS or SERVICE_MEMORY:
        print(f"[agent] Service limits: cpus={SERVICE_CPUS or '—'} memory={SERVICE_MEMORY or '—'}")
    status, resp = _docker("POST", f"/containers/create?name={urllib.parse.quote(name)}", body={
        "Image": image,
        "HostConfig": host_config,
    })
    if status not in (200, 201):
        raise RuntimeError(f"docker create failed: {resp}")
    cid = resp["Id"]

    status, _ = _docker("POST", f"/containers/{cid}/start")
    if status not in (200, 204):
        raise RuntimeError(f"docker start failed: HTTP {status}")

    _service_container_id = cid
    _service_image = image

    _, info = _docker("GET", f"/containers/{urllib.parse.quote(name, safe='')}/json")
    networks = info.get("NetworkSettings", {}).get("Networks", {})
    for net in networks.values():
        ip = net.get("IPAddress", "")
        if ip:
            return ip
    return ""


def _service_is_running() -> bool:
    """Check if the tracked service container is currently running."""
    if not _service_container_id:
        return False
    try:
        status, info = _docker("GET", f"/containers/{_service_container_id}/json", timeout=5)
        return status == 200 and info.get("State", {}).get("Running", False)
    except Exception:
        return False


def apply_service_assignment(image: str, service_port: int) -> None:
    """Run the service container and point the tunnel's local target at it.

    The tunnel client re-reads LOCAL_SERVICE_ADDR for every stream, so writing the
    file is enough — no client restart. That keeps the volunteer's public tunnel
    port stable, so the HAProxy svc_* and volunteers backends (already pointed at
    that port by the registrar at enroll/assign time) stay correct.
    """
    print(f"[agent] Starting service image={image} port={service_port}")
    ip = run_service(image, service_port)
    addr = f"{ip}:{service_port}"
    print(f"[agent] Service container up at {addr}")
    with open(LOCAL_SERVICE_ADDR_FILE, "w") as f:
        f.write(addr)
    print(f"[agent] LOCAL_SERVICE_ADDR set to {addr} — tunnel will use it on the next request")


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

def enroll(tunnel_public_port: int) -> tuple[str, dict | None] | None:
    """Register with the registrar. Returns (volunteer_id, assignment) or None on failure."""
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
        assignment = resp.get("assignment")
        print(f"[agent] Enrolled successfully — volunteer_id={vid}")
        if assignment:
            print(f"[agent] Assignment received — image={assignment.get('image')} port={assignment.get('service_port')}")
        return vid, assignment

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
            "service_running": _service_is_running(),
            "service_image":   _service_image or None,
        }

        resp = post("/heartbeat", payload)

        if resp:
            weight = round(100 - (cpu * 0.6 + mem * 0.4), 1)
            print(f"[agent] Heartbeat sent — cpu={cpu}% mem={mem}% weight={weight}")
            assignment = resp.get("assignment")
            if assignment and not _service_is_running():
                print(f"[agent] Assignment received via heartbeat — image={assignment.get('image')}")
                try:
                    apply_service_assignment(assignment["image"], assignment["service_port"])
                except Exception as e:
                    print(f"[agent] Failed to apply assignment from heartbeat: {e}")

        elif last_status_code() == 404:
            # Registrar lost state (e.g. restarted) — re-enroll.
            print(f"[agent] Registrar doesn't recognise volunteer_id={current_vid} — re-enrolling…")
            backoff = 2
            enroll_result = None
            while enroll_result is None:
                # Re-read the public port in case the tunnel reconnected.
                try:
                    tunnel_public_port = int(open(TUNNEL_PUBLIC_PORT_FILE).read().strip())
                except (FileNotFoundError, ValueError):
                    pass
                enroll_result = enroll(tunnel_public_port)
                if enroll_result is None:
                    print(f"[agent] Re-enrollment failed, retrying in {backoff}s…")
                    time.sleep(backoff)
                    backoff = min(backoff * 2, 30)
            current_vid, assignment = enroll_result
            global VOLUNTEER_ID
            VOLUNTEER_ID = current_vid
            print(f"[agent] Re-enrolled — new volunteer_id={current_vid}")
            if assignment:
                try:
                    apply_service_assignment(assignment["image"], assignment["service_port"])
                except Exception as e:
                    print(f"[agent] Failed to apply service assignment after re-enroll: {e}")

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
    enroll_result = None
    while enroll_result is None:
        enroll_result = enroll(tunnel_public_port)
        if enroll_result is None:
            print(f"[agent] Retrying enrollment in {backoff}s…")
            time.sleep(backoff)
            backoff = min(backoff * 2, 30)

    vid, assignment = enroll_result

    global VOLUNTEER_ID
    VOLUNTEER_ID = vid

    if assignment:
        try:
            apply_service_assignment(assignment["image"], assignment["service_port"])
        except Exception as e:
            print(f"[agent] Failed to apply service assignment: {e}")

    heartbeat_loop(vid, tunnel_public_port)


if __name__ == "__main__":
    main()