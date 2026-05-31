// src/server.rs  — Machine B
//
// Responsibilities:
//   1. Listen for the tunnel agent (machine A) on CONTROL_PORT.
//   2. Once A connects, listen for public visitors on PUBLIC_PORT.
//   3. For every visitor: send OPEN(stream_id) to A over the control channel,
//      then wait for A to dial back a fresh data connection on DATA_PORT.
//   4. Splice bytes between the visitor socket and A's data socket.
//   5. Send periodic PINGs; if A stops responding, drop everything and wait
//      for a fresh control connection.

#![allow(clippy::needless_pass_by_value)]

mod proto;

use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

// ── configuration ────────────────────────────────────────────────────────────

/// Port the tunnel agent (A) dials to register itself.
const CONTROL_PORT: u16 = 9007;

/// Port public visitors connect to.
const PUBLIC_PORT: u16 = 7007;

/// How often the server sends a PING over the control channel.
const PING_INTERVAL: Duration = Duration::from_secs(10);

/// How long we wait for A to open a data connection after an OPEN signal.
const DATA_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);

// ── shared state ─────────────────────────────────────────────────────────────

/// Pending visitor sockets keyed by stream_id.
/// The main accept loop inserts entries; the data-port thread removes them.
type PendingMap = Arc<Mutex<HashMap<u16, TcpStream>>>;

// ── entry point ──────────────────────────────────────────────────────────────

fn main() {
    println!("[server] reverse-tunnel server starting");
    println!("[server]   control port : {CONTROL_PORT}");
    println!("[server]   public port  : {PUBLIC_PORT}");

    // Outer loop: keep waiting for a tunnel agent to connect.
    loop {
        println!("[server] waiting for tunnel agent on port {CONTROL_PORT}…");
        let control = wait_for_agent();
        println!("[server] tunnel agent connected — opening public port {PUBLIC_PORT}");
        run_session(control);
        println!("[server] session ended — restarting\n");
    }
}

// ── wait for the agent ───────────────────────────────────────────────────────

fn wait_for_agent() -> TcpStream {
    let listener = TcpListener::bind(("0.0.0.0", CONTROL_PORT))
        .expect("cannot bind control port");
    let (stream, addr) = listener.accept().expect("accept failed");
    println!("[server] agent connected from {addr}");
    stream
}

// ── run one tunnel session ───────────────────────────────────────────────────

fn run_session(control: TcpStream) {
    // Clone handles for threads that need them.
    let control_read  = control.try_clone().expect("clone failed");
    let control_write = control;

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

    // Thread 1 — read PONG / CLOSE messages from the agent.
    {
        let pending = Arc::clone(&pending);
        thread::spawn(move || control_reader(control_read, pending));
    }

    // Thread 2 — accept data connections from the agent.
    let pending_data = Arc::clone(&pending);
    let mut port_buf = [0u8; 2];
    control.read_exact(&mut port_buf).unwrap();
    let data_port = u16::from_be_bytes(port_buf);
    let data_listener = TcpListener::bind(("0.0.0.0", data_port))
        .expect("cannot bind data port");
    thread::spawn(move || data_acceptor(data_listener, pending_data));

    // Main thread — send PINGs + accept public visitors.
    public_acceptor(control_write, Arc::clone(&pending));
}

// ── control channel reader ───────────────────────────────────────────────────

/// Reads frames from the control socket. Currently handles PONG (silently
/// acknowledges) and CLOSE (removes the pending entry if it is still there).
fn control_reader(mut ctrl: TcpStream, pending: PendingMap) {
    let mut buf = [0u8; proto::FRAME_LEN];
    loop {
        if read_exact_or_eof(&mut ctrl, &mut buf).is_err() {
            println!("[server] control channel closed (reader)");
            return;
        }
        let (tag, stream_id) = proto::decode(&buf);
        match tag {
            proto::TAG_PONG => { /* keepalive ack — nothing to do */ }
            proto::TAG_CLOSE => {
                pending.lock().unwrap().remove(&stream_id);
            }
            other => {
                println!("[server] unexpected tag 0x{other:02x} — ignoring");
            }
        }
    }
}

// ── data connection acceptor ─────────────────────────────────────────────────

/// The agent dials this port once per visitor, immediately after receiving
/// an OPEN frame.  We peek at the stream_id by reading the first 2 bytes the
/// agent sends, find the matching visitor socket, and splice.
fn data_acceptor(listener: TcpListener, pending: PendingMap) {
    // Set a short accept timeout so the thread doesn't block forever if the
    // agent disappears.
    for incoming in listener.incoming() {
        let mut agent_data = match incoming {
            Ok(s) => s,
            Err(e) => { println!("[server] data accept error: {e}"); continue; }
        };

        // The agent writes 2 bytes — the stream_id it is serving.
        let mut id_buf = [0u8; 2];
        if agent_data.read_exact(&mut id_buf).is_err() {
            println!("[server] could not read stream_id from agent data conn");
            continue;
        }
        let stream_id = u16::from_be_bytes(id_buf);

        // Find the visitor socket that is waiting for this stream_id.
        let visitor = pending.lock().unwrap().remove(&stream_id);
        match visitor {
            None => {
                println!("[server] no pending visitor for stream {stream_id} — dropping");
            }
            Some(visitor_sock) => {
                println!("[server] splicing stream {stream_id}");
                thread::spawn(move || splice(agent_data, visitor_sock));
            }
        }
    }
}

// ── public visitor acceptor ──────────────────────────────────────────────────

/// Accepts connections from the internet, assigns stream IDs, sends OPEN
/// frames to the agent, and emits PINGs on a schedule.
fn public_acceptor(mut ctrl: TcpStream, pending: PendingMap) {
    let public_listener = TcpListener::bind(("0.0.0.0", PUBLIC_PORT))
        .expect("cannot bind public port");
    // Non-blocking so we can interleave PING sends.
    public_listener
        .set_nonblocking(true)
        .expect("set_nonblocking failed");

    let mut stream_counter: u16 = 0;
    let mut last_ping = std::time::Instant::now();

    loop {
        // — PING —
        if last_ping.elapsed() >= PING_INTERVAL {
            let frame = proto::encode(proto::TAG_PING, 0);
            if ctrl.write_all(&frame).is_err() {
                println!("[server] control channel write failed (PING)");
                return;
            }
            last_ping = std::time::Instant::now();
        }

        // — accept visitor (non-blocking) —
        match public_listener.accept() {
            Ok((visitor, addr)) => {
                println!("[server] visitor from {addr} → stream {stream_counter}");
                let stream_id = stream_counter;
                stream_counter = stream_counter.wrapping_add(1);

                // Store visitor; give the agent DATA_ACCEPT_TIMEOUT to dial back.
                visitor
                    .set_read_timeout(Some(DATA_ACCEPT_TIMEOUT))
                    .ok();
                pending.lock().unwrap().insert(stream_id, visitor);

                // Signal the agent.
                let frame = proto::encode(proto::TAG_OPEN, stream_id);
                if ctrl.write_all(&frame).is_err() {
                    println!("[server] control channel write failed (OPEN)");
                    return;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No visitor right now — that's fine.
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                println!("[server] public accept error: {e}");
                return;
            }
        }
    }
}

// ── byte splice ──────────────────────────────────────────────────────────────

/// Copies bytes bidirectionally between two TCP streams until either side
/// closes.  Uses two threads — one per direction — joined by waiting on both.
fn splice(a: TcpStream, b: TcpStream) {
    let a_r = a.try_clone().expect("clone a");
    let b_r = b.try_clone().expect("clone b");
    let a_w = a;
    let b_w = b;

    let t1 = thread::spawn(move || copy_half(a_r, b_w, "agent→visitor"));
    let t2 = thread::spawn(move || copy_half(b_r, a_w, "visitor→agent"));
    let _ = t1.join();
    let _ = t2.join();
}

fn copy_half(mut src: TcpStream, mut dst: TcpStream, label: &str) {
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
    // Half-close: signal to the peer that we're done writing, but don't
    // cut off the other direction — let it drain naturally.
    let _ = dst.shutdown(std::net::Shutdown::Write);
    println!("[server] half-pipe {label} closed");
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn read_exact_or_eof(stream: &mut TcpStream, buf: &mut [u8]) -> io::Result<()> {
    match stream.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) => Err(e),
    }
}