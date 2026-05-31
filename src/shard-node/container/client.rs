// src/client.rs  — Machine A
//
// Responsibilities:
//   1. Connect to the server's CONTROL_PORT and keep that socket alive.
//   2. Respond to PING with PONG.
//   3. On OPEN(stream_id): open a fresh TCP connection to SERVER:DATA_PORT,
//      send the stream_id as the first 2 bytes, then connect to the local
//      service and splice.
//   4. If the control connection drops, wait briefly and reconnect.

mod proto;

use std::{
    io::{Read, Write},
    net::TcpStream,
    thread,
    time::Duration,
    env
};

// ── configuration ────────────────────────────────────────────────────────────

/// Address of machine B.
const SERVER_ADDR: &str = "127.0.0.1"; // ← replace with B's public IP/hostname

/// Port on B that accepts the control connection.
const CONTROL_PORT: u16 = 9007;

/// The local service on machine A we want to expose.
const LOCAL_SERVICE_ADDR: &str = "127.0.0.1";
const LOCAL_SERVICE_PORT: u16  = 8080; // ← replace with your actual service port

/// How long to wait before reconnecting after a dropped control channel.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

// ── environment ───────────────────────────────────────────────────────────────

/// Read DATA_PORT from the TUNNEL_DATA_PORT environment variable.
/// Exits the process with a clear error if the variable is absent or invalid.
fn require_data_port() -> u16 {
    match env::var("TUNNEL_DATA_PORT") {
        Err(_) => {
            eprintln!("[client] FATAL: TUNNEL_DATA_PORT is not set.");
            eprintln!("[client]   Set it to the data port on the tunnel server, e.g.:");
            eprintln!("[client]   export TUNNEL_DATA_PORT=9008");
            std::process::exit(1);
        }
        Ok(val) => {
            val.trim().parse::<u16>().unwrap_or_else(|_| {
                eprintln!(
                    "[client] FATAL: TUNNEL_DATA_PORT={val:?} is not a valid port number (1-65535)."
                );
                std::process::exit(1);
            })
        }
    }
}


// ── entry point ──────────────────────────────────────────────────────────────

fn main() {
    // Block immediately if DATA_PORT is not configured.
    let data_port: u16 = require_data_port();

    println!("[client] reverse-tunnel agent starting");
    println!("[client]   server       : {SERVER_ADDR}");
    println!("[client]   control port : {CONTROL_PORT}");
    println!("[client]   data port    : {data_port}  (from TUNNEL_DATA_PORT)");
    println!("[client]   local service: {LOCAL_SERVICE_ADDR}:{LOCAL_SERVICE_PORT}");

    loop {
        println!("[client] connecting control channel to {SERVER_ADDR}:{CONTROL_PORT}...");
        match TcpStream::connect((SERVER_ADDR, CONTROL_PORT)) {
            Err(e) => {
                println!("[client] connection failed: {e} — retrying in {RECONNECT_DELAY:?}");
                thread::sleep(RECONNECT_DELAY);
            }
            Ok(mut ctrl) => {
                println!("[client] control channel established");
                let port_bytes = data_port.to_be_bytes();
                ctrl.write_all(&port_bytes).unwrap();
                run_session(ctrl, data_port);
                println!("[client] session ended — reconnecting in {RECONNECT_DELAY:?}\n");
                thread::sleep(RECONNECT_DELAY);
            }
        }
    }
}

// ── session loop ─────────────────────────────────────────────────────────────

/// Reads frames from the control channel forever.
/// PING  → reply PONG (in-place, same thread — fast enough for keepalive).
/// OPEN  → spawn a thread to handle the new stream.
/// CLOSE → nothing to do on the client side for now.
fn run_session(mut ctrl: TcpStream, data_port: u16) {
    // Give us a write-clone so the reader loop can reply with PONG without
    // extra locking ceremony.
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
                let pong = proto::encode(proto::TAG_PONG, 0);
                if ctrl_write.write_all(&pong).is_err() {
                    println!("[client] control channel write error — session over");
                    return;
                }
            }
            proto::TAG_OPEN => {
                println!("[client] OPEN for stream {stream_id} — spawning data thread");
                thread::spawn(move || handle_stream(stream_id, data_port));
            }
            proto::TAG_CLOSE => {
                // Server is telling us it already closed — nothing to do.
                println!("[client] CLOSE for stream {stream_id}");
            }
            other => {
                println!("[client] unknown tag 0x{other:02x} — ignoring");
            }
        }
    }
}

// ── per-stream handler ────────────────────────────────────────────────────────

/// Called in its own thread for each incoming visitor.
///
///  1. Open a data connection to B:DATA_PORT.
///  2. Send stream_id (2 bytes) so B knows which visitor to pair us with.
///  3. Open a connection to the local service.
///  4. Splice the two sockets.
fn handle_stream(stream_id: u16, data_port: u16) {
    // Step 1 & 2 — connect to server data port and identify ourselves.
    let mut server_data = match TcpStream::connect((SERVER_ADDR, data_port)) {
        Ok(s) => s,
        Err(e) => {
            println!("[client] stream {stream_id}: cannot connect to data port: {e}");
            return;
        }
    };
    let id_bytes = stream_id.to_be_bytes();
    if server_data.write_all(&id_bytes).is_err() {
        println!("[client] stream {stream_id}: failed to send stream_id");
        return;
    }

    // Step 3 — connect to the local service.
    let local = match TcpStream::connect((LOCAL_SERVICE_ADDR, LOCAL_SERVICE_PORT)) {
        Ok(s) => s,
        Err(e) => {
            println!("[client] stream {stream_id}: cannot reach local service: {e}");
            return;
        }
    };

    // Step 4 — splice.
    println!("[client] stream {stream_id}: splicing");
    splice(server_data, local, stream_id);
    println!("[client] stream {stream_id}: done");
}

// ── byte splice ───────────────────────────────────────────────────────────────

fn splice(a: TcpStream, b: TcpStream, stream_id: u16) {
    let a_r = a.try_clone().expect("clone");
    let b_r = b.try_clone().expect("clone");
    let a_w = a;
    let b_w = b;

    let id = stream_id;
    let t1 = thread::spawn(move || copy_half(a_r, b_w, id, "server→local"));
    let t2 = thread::spawn(move || copy_half(b_r, a_w, id, "local→server"));
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
        if dst.write_all(&buf[..n]).is_err() {
            break;
        }
    }
    let _ = dst.shutdown(std::net::Shutdown::Write);
    println!("[client] stream {stream_id} half-pipe {label} closed");
}