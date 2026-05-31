#!/usr/bin/env python3
"""
OpenShard Volunteer Agent
Handles enrollment and heartbeat with the registrar.
"""

import os
import time
import json
import socket
import platform
import urllib.request
import urllib.error
import shutil


# ── Config from environment ───────────────────────────────────────────────────
REGISTRAR_URL       = os.environ.get("REGISTRAR_URL", "http://localhost:3000")
SERVICE_PORT        = int(os.environ.get("SERVICE_PORT", "8080"))
HEARTBEAT_INTERVAL  = int(os.environ.get("HEARTBEAT_INTERVAL", "15"))
HOSTNAME            = os.environ.get("HOSTNAME", socket.gethostname())
TUNNEL_VERSION      = "0.1.0"

volunteer_id = None


# ── Metrics ───────────────────────────────────────────────────────────────────
def read_cpu_pct() -> float:
    """Read CPU usage from /proc/stat over a 500ms window."""
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
    """Read memory usage from /proc/meminfo."""
    info = {}
    with open("/proc/meminfo") as f:
        for line in f:
            key, val = line.split(":")
            info[key.strip()] = int(val.strip().split()[0])

    total     = info.get("MemTotal", 1)
    available = info.get("MemAvailable", 0)
    used      = total - available
    return round((used / total) * 100.0, 2)


def read_load_avg() -> float:
    """Read 1-minute load average from /proc/loadavg."""
    with open("/proc/loadavg") as f:
        return float(f.read().split()[0])


def get_docker_version() -> str:
    """Return docker version if available, else unknown."""
    if shutil.which("docker"):
        try:
            import subprocess
            result = subprocess.run(
                ["docker", "--version"],
                capture_output=True, text=True, timeout=2
            )
            return result.stdout.strip().split()[2].rstrip(",")
        except Exception:
            pass
    return "unknown"

# ── Last status code tracker ──────────────────────────────────────────────────
_last_status_code = 0

def last_status_code() -> int:
    return _last_status_code

# ── HTTP helpers ──────────────────────────────────────────────────────────────
def post(path: str, payload: dict) -> dict | None:
    url  = f"{REGISTRAR_URL}{path}"
    data = json.dumps(payload).encode()
    req  = urllib.request.Request(
        url, data=data,
        headers={"Content-Type": "application/json"},
        method="POST"
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            _last_status_code = resp.status
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        print(f"[agent] HTTP error {e.code} on {path}: {e.read().decode()}")
    except urllib.error.URLError as e:
        print(f"[agent] Connection error on {path}: {e.reason}")
    except Exception as e:
        print(f"[agent] Unexpected error on {path}: {e}")
    return None


# ── Enrollment ────────────────────────────────────────────────────────────────
def enroll() -> str | None:
    """Register with the registrar. Returns volunteer_id or None on failure."""
    payload = {
        "os":               f"{platform.system()} {platform.release()}",
        "arch":             platform.machine(),
        "cpu_cores":        os.cpu_count() or 1,
        "cpu_model":        "unknown",
        "memory_total_mb":  _total_mem_mb(),
        "disk_free_gb":     _disk_free_gb(),
        "docker_version":   get_docker_version(),
        "tunnel_version":   TUNNEL_VERSION,
        "service_addr":     SERVICE_ADDR,
    }

    print(f"[agent] Enrolling → {payload['service_addr']}")
    resp = post("/enroll", payload)

    if resp and "port_id" in resp and "volunteer_id" in resp:
        port_id = resp["port_id"]
        vid = resp["volunteer_id"]
        print(f"[agent] Enrolled successfully. port_id={port_id}, vid={vid}")
        os.environ["TUNNEL_DATA_PORT"] = f"{int(port_id)}"
        return vid

    print("[agent] Enrollment failed.")
    return None


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


# ── Heartbeat loop ────────────────────────────────────────────────────────────
def heartbeat_loop(vid: str):
    """Send heartbeat every HEARTBEAT_INTERVAL seconds."""
    current_vid = vid

    while True:
        time.sleep(HEARTBEAT_INTERVAL)

        try:
            cpu = read_cpu_pct()
            mem = read_mem_pct()
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
            # Registrar lost state (restarted) — re-enroll
            print(f"[agent] Registrar does not recognize volunteer_id={current_vid} — re-enrolling...")
            backoff = 2
            new_vid = None
            while new_vid is None:
                new_vid = enroll()
                if new_vid is None:
                    print(f"[agent] Re-enrollment failed, retrying in {backoff}s...")
                    time.sleep(backoff)
                    backoff = min(backoff * 2, 30)
            current_vid = new_vid
            print(f"[agent] Re-enrolled successfully. new volunteer_id={current_vid}")

        else:
            print("[agent] Heartbeat failed — registrar unreachable, will retry")


# ── Main ──────────────────────────────────────────────────────────────────────
def main():
    global SERVICE_ADDR
    SERVICE_ADDR = f"{os.environ.get('HOST_IP', _local_ip())}:{SERVICE_PORT}"
    print("[agent] OpenShard Volunteer Agent starting")
    print(f"[agent] Registrar: {REGISTRAR_URL}")
    print(f"[agent] Service:   :{SERVICE_PORT}")

    # Retry enrollment with backoff
    backoff = 2
    vid = None
    while vid is None:
        vid = enroll()
        if vid is None:
            print(f"[agent] Retrying enrollment in {backoff}s...")
            time.sleep(backoff)
            backoff = min(backoff * 2, 30)

    heartbeat_loop(vid)


if __name__ == "__main__":
    main()