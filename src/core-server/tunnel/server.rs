// src/server.rs  — Machine B
//
// Responsibilities:
//   1. Listen for tunnel agents (machine A) on CONTROL_PORT.
//   2. For each agent: allocate a public port AND a data port from the pool,
//      bind both, send 4 bytes back (public_port, data_port).
//   3. For every visitor on that public port: send OPEN(stream_id) to the
//      agent, wait for it to dial back on the data port, then splice.
//   4. Send periodic PINGs; drop the session if the agent stops responding.
//   5. On session end, release both ports back to the pool.
//
// The server owns all port allocation — the client sends nothing during
// the handshake, only reads.

#![allow(clippy::needless_pass_by_value)]

mod proto;

use std::{
    collections::{HashMap, HashSet},
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

// ── configuration ────────────────────────────────────────────────────────────

/// Port tunnel agents dial to register themselves.
const CONTROL_PORT: u16 = 9007;

/// Each agent gets one public port (HAProxy routes here) and one data port
/// (the agent dials back here per stream).  Both come from the same pool.
const PORT_RANGE_START: u16 = 30000;
const PORT_RANGE_END:   u16 = 31000;

/// How often the server sends a PING over the control channel.
const PING_INTERVAL: Duration = Duration::from_secs(10);

/// How long we wait for the agent to open a data connection after OPEN.
const DATA_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);

// ── shared state ─────────────────────────────────────────────────────────────

type PendingMap = Arc<Mutex<HashMap<u16, TcpStream>>>;
type PortSet    = Arc<Mutex<HashSet<u16>>>;

// ── entry point ──────────────────────────────────────────────────────────────

fn main() {
    println!("[server] reverse-tunnel server starting");
    println!("[server]   control port : {CONTROL_PORT}");
    println!("[server]   port range   : {PORT_RANGE_START}-{PORT_RANGE_END}");

    let used_ports: PortSet = Arc::new(Mutex::new(HashSet::new()));

    let control_listener = TcpListener::bind(("0.0.0.0", CONTROL_PORT))
        .expect("cannot bind control port");

    println!("[server] waiting for agents…");

    for incoming in control_listener.incoming() {
        match incoming {
            Err(e) => println!("[server] control accept error: {e}"),
            Ok(stream) => {
                let addr = stream.peer_addr()
                    .map_or_else(|_| "unknown".to_string(), |a| a.to_string());
                println!("[server] agent connected from {addr}");
                let used_ports = Arc::clone(&used_ports);
                thread::spawn(move || run_session(stream, used_ports));
            }
        }
    }
}

// ── port allocation ───────────────────────────────────────────────────────────

/// Bind to a free port in the pool and mark it used.
/// Returns (TcpListener, port) or None if exhausted.
fn allocate_port(used_ports: &PortSet) -> Option<(TcpListener, u16)> {
    let mut used = used_ports.lock().unwrap();
    for port in PORT_RANGE_START..=PORT_RANGE_END {
        if used.contains(&port) {
            continue;
        }
        if let Ok(listener) = TcpListener::bind(("0.0.0.0", port)) {
            used.insert(port);
            return Some((listener, port));
        }
    }
    None
}

fn release_port(used_ports: &PortSet, port: u16) {
    used_ports.lock().unwrap().remove(&port);
}

// ── run one agent session ─────────────────────────────────────────────────────

fn run_session(mut control: TcpStream, used_ports: PortSet) {
    // Allocate public port (HAProxy backend) and data port (agent dials back).
    let (public_listener, public_port) = match allocate_port(&used_ports) {
        Some(p) => p,
        None => { println!("[server] no free ports for public — rejecting"); return; }
    };
    let (data_listener, data_port) = match allocate_port(&used_ports) {
        Some(p) => p,
        None => {
            println!("[server] no free ports for data — rejecting");
            release_port(&used_ports, public_port);
            return;
        }
    };

    println!("[server] assigned public={public_port} data={data_port}");

    // Send both ports to the client: [public_hi, public_lo, data_hi, data_lo]
    let handshake = [
        (public_port >> 8) as u8, (public_port & 0xff) as u8,
        (data_port   >> 8) as u8, (data_port   & 0xff) as u8,
    ];
    if control.write_all(&handshake).is_err() {
        println!("[server] failed to send handshake — dropping");
        release_port(&used_ports, public_port);
        release_port(&used_ports, data_port);
        return;
    }

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

    // Thread 1 — read PONG / CLOSE frames from the agent.
    {
        let ctrl_read = match control.try_clone() {
            Ok(c) => c,
            Err(e) => {
                println!("[server] clone failed: {e}");
                release_port(&used_ports, public_port);
                release_port(&used_ports, data_port);
                return;
            }
        };
        let pending = Arc::clone(&pending);
        thread::spawn(move || control_reader(ctrl_read, pending));
    }

    // Thread 2 — accept data connections from the agent.
    {
        let pending = Arc::clone(&pending);
        thread::spawn(move || data_acceptor(data_listener, pending));
    }

    // Main thread — accept public visitors + send PINGs.
    public_acceptor(control, public_listener, public_port, Arc::clone(&pending));

    release_port(&used_ports, public_port);
    release_port(&used_ports, data_port);
    println!("[server] session ended — ports {public_port} and {data_port} released");
}

// ── control channel reader ────────────────────────────────────────────────────

fn control_reader(mut ctrl: TcpStream, pending: PendingMap) {
    let mut buf = [0u8; proto::FRAME_LEN];
    loop {
        if ctrl.read_exact(&mut buf).is_err() {
            println!("[server] control channel closed (reader)");
            return;
        }
        let (tag, stream_id) = proto::decode(&buf);
        match tag {
            proto::TAG_PONG  => {}
            proto::TAG_CLOSE => { pending.lock().unwrap().remove(&stream_id); }
            other => println!("[server] unexpected tag 0x{other:02x} — ignoring"),
        }
    }
}

// ── data connection acceptor ──────────────────────────────────────────────────

fn data_acceptor(listener: TcpListener, pending: PendingMap) {
    for incoming in listener.incoming() {
        let mut agent_data = match incoming {
            Ok(s)  => s,
            Err(e) => { println!("[server] data accept error: {e}"); continue; }
        };

        let mut id_buf = [0u8; 2];
        if agent_data.read_exact(&mut id_buf).is_err() {
            println!("[server] could not read stream_id from agent data conn");
            continue;
        }
        let stream_id = u16::from_be_bytes(id_buf);

        match pending.lock().unwrap().remove(&stream_id) {
            None => println!("[server] no pending visitor for stream {stream_id} — dropping"),
            Some(visitor_sock) => {
                println!("[server] splicing stream {stream_id}");
                thread::spawn(move || splice(agent_data, visitor_sock));
            }
        }
    }
}

// ── public visitor acceptor ───────────────────────────────────────────────────

fn public_acceptor(
    mut ctrl: TcpStream,
    public_listener: TcpListener,
    public_port: u16,
    pending: PendingMap,
) {
    public_listener.set_nonblocking(true).expect("set_nonblocking failed");

    let mut stream_counter: u16 = 0;
    let mut last_ping = std::time::Instant::now();

    loop {
        if last_ping.elapsed() >= PING_INTERVAL {
            if ctrl.write_all(&proto::encode(proto::TAG_PING, 0)).is_err() {
                println!("[server] port {public_port}: PING failed — ending session");
                return;
            }
            last_ping = std::time::Instant::now();
        }

        match public_listener.accept() {
            Ok((visitor, addr)) => {
                let stream_id = stream_counter;
                stream_counter = stream_counter.wrapping_add(1);
                println!("[server] port {public_port}: visitor from {addr} → stream {stream_id}");

                visitor.set_read_timeout(Some(DATA_ACCEPT_TIMEOUT)).ok();
                pending.lock().unwrap().insert(stream_id, visitor);

                if ctrl.write_all(&proto::encode(proto::TAG_OPEN, stream_id)).is_err() {
                    println!("[server] port {public_port}: OPEN failed — ending session");
                    return;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                println!("[server] port {public_port}: accept error: {e}");
                return;
            }
        }
    }
}

// ── byte splice ───────────────────────────────────────────────────────────────

fn splice(a: TcpStream, b: TcpStream) {
    let a_r = a.try_clone().expect("clone a");
    let b_r = b.try_clone().expect("clone b");
    let t1 = thread::spawn(move || copy_half(a_r, b,  "agent→visitor"));
    let t2 = thread::spawn(move || copy_half(b_r, a,  "visitor→agent"));
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
        if dst.write_all(&buf[..n]).is_err() { break; }
    }
    let _ = dst.shutdown(std::net::Shutdown::Write);
    println!("[server] half-pipe {label} closed");
}