// src/client.rs  — Machine A
//
// Responsibilities:
//   1. Connect to the server's CONTROL_PORT.
//   2. Read 4 bytes back: [public_port (2), data_port (2)] — server owns
//      all allocation, client sends nothing during handshake.
//   3. Write public_port to /tmp/tunnel_public_port for the Python agent.
//   4. Respond to PING with PONG.
//   5. On OPEN(stream_id): dial SERVER:data_port, send stream_id (2 bytes),
//      connect to local service, splice.
//   6. On session end, clean up the port file and reconnect.

mod proto;

use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    thread,
    time::Duration,
};

// ── configuration ────────────────────────────────────────────────────────────

const CONTROL_PORT:             u16      = 9007;
const RECONNECT_DELAY:          Duration = Duration::from_secs(5);
const LOCAL_SERVICE_ADDR_FILE:  &str     = "/tmp/tunnel_local_addr";

fn server_addr() -> String {
    std::env::var("TUNNEL_SERVER").unwrap_or_else(|_| "tunnel".to_string())
}

/// Returns the local service address as "host:port".
/// Priority: LOCAL_SERVICE_ADDR env var → /tmp/tunnel_local_addr file → default 127.0.0.1:8080.
/// Called on every reconnect loop so a new address written by the agent takes effect on next session.
fn local_service_addr() -> String {
    if let Ok(v) = std::env::var("LOCAL_SERVICE_ADDR") {
        let v = v.trim().to_string();
        if !v.is_empty() { return v; }
    }
    if let Ok(v) = fs::read_to_string(LOCAL_SERVICE_ADDR_FILE) {
        let v = v.trim().to_string();
        if !v.is_empty() { return v; }
    }
    "127.0.0.1:8080".to_string()
}

/// Written after a successful handshake so the Python agent can enroll.
const PUBLIC_PORT_FILE: &str = "/tmp/tunnel_public_port";

// ── entry point ──────────────────────────────────────────────────────────────

fn main() {
    let server = server_addr();
    println!("[client] reverse-tunnel agent starting");
    println!("[client]   server: {server}:{CONTROL_PORT}");

    loop {
        let local_addr = local_service_addr();
        println!("[client] connecting to {server}:{CONTROL_PORT}… (local service: {local_addr})");
        match TcpStream::connect((server.as_str(), CONTROL_PORT)) {
            Err(e) => {
                println!("[client] connection failed: {e} — retrying in {RECONNECT_DELAY:?}");
                thread::sleep(RECONNECT_DELAY);
            }
            Ok(mut ctrl) => {
                println!("[client] control channel established");

                // Read 4-byte handshake: public_port (2) + data_port (2).
                let mut buf = [0u8; 4];
                if ctrl.read_exact(&mut buf).is_err() {
                    println!("[client] failed to read handshake — retrying");
                    thread::sleep(RECONNECT_DELAY);
                    continue;
                }
                let public_port = u16::from_be_bytes([buf[0], buf[1]]);
                let data_port   = u16::from_be_bytes([buf[2], buf[3]]);
                println!("[client] public_port={public_port} data_port={data_port}");

                // Write public port for the Python agent to pick up.
                if let Err(e) = fs::write(PUBLIC_PORT_FILE, public_port.to_string()) {
                    println!("[client] failed to write public port file: {e} — retrying");
                    thread::sleep(RECONNECT_DELAY);
                    continue;
                }

                run_session(ctrl, data_port, &local_addr);

                // Remove the port file so the Python agent doesn't use a
                // stale value if we reconnect with a different port.
                let _ = fs::remove_file(PUBLIC_PORT_FILE);
                println!("[client] session ended — reconnecting in {RECONNECT_DELAY:?}\n");
                thread::sleep(RECONNECT_DELAY);
            }
        }
    }
}

// ── session loop ──────────────────────────────────────────────────────────────

fn run_session(mut ctrl: TcpStream, data_port: u16, local_addr: &str) {
    let mut ctrl_write = match ctrl.try_clone() {
        Ok(c) => c,
        Err(e) => { println!("[client] clone failed: {e}"); return; }
    };

    let mut buf = [0u8; proto::FRAME_LEN];
    loop {
        if ctrl.read_exact(&mut buf).is_err() {
            println!("[client] control channel read error — session over");
            return;
        }
        let (tag, stream_id) = proto::decode(&buf);
        match tag {
            proto::TAG_PING => {
                if ctrl_write.write_all(&proto::encode(proto::TAG_PONG, 0)).is_err() {
                    println!("[client] control write error — session over");
                    return;
                }
            }
            proto::TAG_OPEN => {
                println!("[client] OPEN stream {stream_id}");
                let addr = local_addr.to_string();
                thread::spawn(move || handle_stream(stream_id, data_port, addr));
            }
            proto::TAG_CLOSE => {
                println!("[client] CLOSE stream {stream_id}");
            }
            other => println!("[client] unknown tag 0x{other:02x} — ignoring"),
        }
    }
}

// ── per-stream handler ────────────────────────────────────────────────────────

fn handle_stream(stream_id: u16, data_port: u16, local_addr: String) {
    let server = server_addr();
    let mut server_data = match TcpStream::connect((server.as_str(), data_port)) {
        Ok(s)  => s,
        Err(e) => { println!("[client] stream {stream_id}: data port connect failed: {e}"); return; }
    };
    if server_data.write_all(&stream_id.to_be_bytes()).is_err() {
        println!("[client] stream {stream_id}: failed to send stream_id");
        return;
    }

    let local = match TcpStream::connect(local_addr.as_str()) {
        Ok(s)  => s,
        Err(e) => { println!("[client] stream {stream_id}: local service connect failed: {e}"); return; }
    };

    println!("[client] stream {stream_id}: splicing");
    splice(server_data, local, stream_id);
    println!("[client] stream {stream_id}: done");
}

// ── byte splice ───────────────────────────────────────────────────────────────

fn splice(a: TcpStream, b: TcpStream, stream_id: u16) {
    let a_r = a.try_clone().expect("clone");
    let b_r = b.try_clone().expect("clone");
    let t1 = thread::spawn(move || copy_half(a_r, b,  stream_id, "server→local"));
    let t2 = thread::spawn(move || copy_half(b_r, a,  stream_id, "local→server"));
    let _ = t1.join();
    let _ = t2.join();
}

fn copy_half(mut src: TcpStream, mut dst: TcpStream, stream_id: u16, label: &str) {
    let mut buf = [0u8; 8192];
    loop {
        let n = match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if dst.write_all(&buf[..n]).is_err() { break; }
    }
    let _ = dst.shutdown(std::net::Shutdown::Write);
    println!("[client] stream {stream_id} half-pipe {label} closed");
}