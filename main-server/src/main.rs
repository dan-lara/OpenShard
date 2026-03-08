use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{any, get},
    Router,
};
use dashmap::DashMap;
use prost::Message;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::timeout,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    transport::Server, Request as TonicRequest, Response as TonicResponse, Status, Streaming,
};
use tracing::{error, info, warn};
use uuid::Uuid;

use tunnel_proto::tunnel::{
    tunnel_service_server::{TunnelService, TunnelServiceServer},
    FrameType, HeartbeatPayload, HttpHeaders, HttpRequestPayload, HttpResponsePayload, TunnelFrame,
};

#[derive(Debug, Clone)]
struct VolunteerNode {
    id: String,
    tx: mpsc::Sender<Result<TunnelFrame, Status>>,
    last_heartbeat: Instant,
    cpu_usage: f32,
    memory_used: u64,
    active_requests: u32,
    load_average: f32,
}

struct AppState {
    volunteers: DashMap<String, VolunteerNode>,
    pending_requests: DashMap<String, oneshot::Sender<TunnelFrame>>,
}

#[derive(Clone)]
struct SharedState(Arc<AppState>);

#[derive(Clone)]
struct ServerImpl {
    state: SharedState,
}

#[tonic::async_trait]
impl TunnelService for ServerImpl {
    type OpenTunnelStream = ReceiverStream<Result<TunnelFrame, Status>>;

    async fn open_tunnel(
        &self,
        request: TonicRequest<Streaming<TunnelFrame>>,
    ) -> Result<TonicResponse<Self::OpenTunnelStream>, Status> {
        let mut stream = request.into_inner();
        let (tx, rx) = mpsc::channel(100);
        let state = self.state.clone();

        // 1. Wait for REGISTER frame
        let first_frame = match stream.message().await {
            Ok(Some(frame)) => frame,
            Ok(None) => return Err(Status::invalid_argument("Stream closed before REGISTER")),
            Err(e) => return Err(e),
        };

        if first_frame.frame_type() != FrameType::Register {
            return Err(Status::invalid_argument("First frame must be REGISTER"));
        }

        let volunteer_id = first_frame.volunteer_id.clone();
        info!("Volunteer {} registered", volunteer_id);

        let node = VolunteerNode {
            id: volunteer_id.clone(),
            tx: tx.clone(),
            last_heartbeat: Instant::now(),
            cpu_usage: 0.0,
            memory_used: 0,
            active_requests: 0,
            load_average: 0.0,
        };

        state.0.volunteers.insert(volunteer_id.clone(), node);

        // 2. Spawn a task to handle incoming frames from this volunteer
        let state_clone = state.clone();
        let vol_id = volunteer_id.clone();
        tokio::spawn(async move {
            while let Ok(Some(frame)) = stream.message().await {
                match frame.frame_type() {
                    FrameType::Heartbeat => {
                        if let Ok(hb) = HeartbeatPayload::decode(frame.payload.as_slice()) {
                            info!(
                                "Heartbeat from {}: CPU={:.1}%, Mem={}B, load={:.2}",
                                frame.volunteer_id, hb.cpu_usage, hb.memory_used, hb.load_average
                            );
                            if let Some(mut n) = state_clone.0.volunteers.get_mut(&frame.volunteer_id) {
                                n.last_heartbeat = Instant::now();
                                n.cpu_usage = hb.cpu_usage;
                                n.memory_used = hb.memory_used;
                                n.active_requests = hb.active_requests;
                                n.load_average = hb.load_average;
                            }
                        }
                    }
                    FrameType::Response => {
                        // Resolve pending HTTP request
                        if let Some((_, sender)) = state_clone.0.pending_requests.remove(&frame.request_id) {
                            let _ = sender.send(frame);
                        } else {
                            warn!("Received RESPONSE for unknown request_id: {}", frame.request_id);
                        }
                    }
                    _ => warn!("Unexpected frame type from volunteer {}", frame.volunteer_id),
                }
            }
            // Stream closed
            info!("Volunteer {} disconnected", vol_id);
            state_clone.0.volunteers.remove(&vol_id);
        });

        Ok(TonicResponse::new(ReceiverStream::new(rx)))
    }
}

// HTTP Dispatch Handler
#[derive(serde::Serialize)]
struct VolunteerStatus {
    id: String,
    last_heartbeat_seconds_ago: u64,
    cpu_usage: f32,
    memory_used: u64,
    active_requests: u32,
    load_average: f32,
}

async fn list_volunteers(State(state): State<SharedState>) -> impl IntoResponse {
    let mut volunteers = Vec::new();
    let now = Instant::now();
    for entry in state.0.volunteers.iter() {
        let node = entry.value();
        volunteers.push(VolunteerStatus {
            id: node.id.clone(),
            last_heartbeat_seconds_ago: now.duration_since(node.last_heartbeat).as_secs(),
            cpu_usage: node.cpu_usage,
            memory_used: node.memory_used,
            active_requests: node.active_requests,
            load_average: node.load_average,
        });
    }
    axum::Json(volunteers)
}

async fn dashboard() -> axum::response::Html<&'static str> {
    axum::response::Html(r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>OpenShard Dashboard</title>
    <style>
        body { font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif; background: #0b0c10; color: #c5c6c7; margin: 0; padding: 2rem; }
        .container { max-width: 1200px; margin: 0 auto; }
        h1 { font-weight: 300; border-bottom: 2px solid #1f2833; padding-bottom: 0.5rem; color: #66fcf1; }
        table { width: 100%; border-collapse: collapse; margin-top: 1.5rem; background: #1f2833; border-radius: 8px; overflow: hidden; box-shadow: 0 4px 6px rgba(0,0,0,0.3); }
        th, td { text-align: left; padding: 1.2rem 1rem; border-bottom: 1px solid #111; }
        th { background: #0b0c10; font-weight: 600; text-transform: uppercase; font-size: 0.85rem; letter-spacing: 0.05em; color: #45a29e; }
        tr:last-child td { border-bottom: none; }
        tr:hover { background: #2b3644; }
        .status-dot { display: inline-block; width: 10px; height: 10px; border-radius: 50%; background: #4caf50; margin-right: 8px; box-shadow: 0 0 5px #4caf50; }
        .status-dot.offline { background: #f44336; box-shadow: 0 0 5px #f44336; }
        .mono { font-family: 'Courier New', Courier, monospace; font-size: 0.9em; color: #66fcf1; background: #0b0c10; padding: 0.2rem 0.5rem; border-radius: 4px; }
    </style>
</head>
<body>
    <div class="container">
        <h1>OpenShard Active Volunteer Nodes</h1>
        <table>
            <thead>
                <tr>
                    <th>Status</th>
                    <th>Node ID</th>
                    <th>Last Seen</th>
                    <th>CPU Usage</th>
                    <th>Memory</th>
                    <th>Active Req</th>
                    <th>Load</th>
                </tr>
            </thead>
            <tbody id="volunteers">
                <tr><td colspan="7" style="text-align: center;">Initializing Secure Connection...</td></tr>
            </tbody>
        </table>
    </div>

    <script>
        async function fetchVolunteers() {
            try {
                const res = await fetch('/api/volunteers');
                const data = await res.json();
                const tbody = document.getElementById('volunteers');
                if (data.length === 0) {
                    tbody.innerHTML = '<tr><td colspan="7" style="text-align: center; color: #888;">No volunteer nodes currently connected. Start an agent to begin!</td></tr>';
                    return;
                }
                
                tbody.innerHTML = data.map(v => {
                    const isOnline = v.last_heartbeat_seconds_ago < 60;
                    const dotClass = isOnline ? 'status-dot' : 'status-dot offline';
                    const memMb = (v.memory_used / 1024 / 1024).toFixed(1);
                    return `
                        <tr>
                            <td><span class="${dotClass}"></span>${isOnline ? 'Online' : 'Timeout'}</td>
                            <td><span class="mono">${v.id.substring(0, 18)}...</span></td>
                            <td>${v.last_heartbeat_seconds_ago}s ago</td>
                            <td>${v.cpu_usage.toFixed(1)}%</td>
                            <td>${memMb} MB</td>
                            <td>${v.active_requests}</td>
                            <td>${v.load_average.toFixed(2)}</td>
                        </tr>
                    `;
                }).join('');
            } catch (err) {
                console.error('Failed to fetch volunteer data', err);
            }
        }
        
        // Refresh every 1000ms
        setInterval(fetchVolunteers, 1000);
        fetchVolunteers();
    </script>
</body>
</html>
    "#)
}

async fn dispatch_request(State(state): State<SharedState>, req: Request<Body>) -> impl IntoResponse {
    // 1. Select the volunteer with the lowest CPU usage (basic scheduler)
    let selected_volunteer = {
        let mut best: Option<VolunteerNode> = None;
        for entry in state.0.volunteers.iter() {
            let node = entry.value();
            if node.last_heartbeat.elapsed() > Duration::from_secs(30) {
                continue; // Ignore offline volunteers
            }
            if best.as_ref().map_or(true, |b| node.cpu_usage < b.cpu_usage) {
                best = Some(node.clone());
            }
        }
        best
    };

    let volunteer = match selected_volunteer {
        Some(v) => v,
        None => return (StatusCode::SERVICE_UNAVAILABLE, "No healthy volunteers available").into_response(),
    };

    let request_id = Uuid::new_v4().to_string();

    // 2. Read the request body and construct the HttpRequestPayload
    use http_body_util::BodyExt;
    let (parts, body) = req.into_parts();
    let body_bytes = match body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return (StatusCode::BAD_REQUEST, "Failed to read body").into_response(),
    };

    let mut headers = std::collections::HashMap::new();
    for (k, v) in parts.headers.iter() {
        if let Ok(v_str) = v.to_str() {
            headers.insert(k.as_str().to_string(), v_str.to_string());
        }
    }

    let payload = HttpRequestPayload {
        method: parts.method.to_string(),
        uri: parts.uri.to_string(), // In full implementation, forward proper path and query
        headers: Some(HttpHeaders { headers }),
        body: body_bytes.to_vec(),
    };

    let mut payload_buf = Vec::new();
    payload.encode(&mut payload_buf).unwrap();

    let frame = TunnelFrame {
        volunteer_id: volunteer.id.clone(),
        request_id: request_id.clone(),
        frame_type: FrameType::Request.into(),
        payload: payload_buf,
    };

    // 3. Register the pending request
    let (resp_tx, resp_rx) = oneshot::channel();
    state.0.pending_requests.insert(request_id.clone(), resp_tx);

    // 4. Send the frame to the volunteer
    if let Err(e) = volunteer.tx.send(Ok(frame)).await {
        state.0.pending_requests.remove(&request_id);
        error!("Failed to send request frame: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "Volunteer disconnected").into_response();
    }

    // 5. Wait for the response frame with a timeout
    let response_frame = match timeout(Duration::from_secs(15), resp_rx).await {
        Ok(Ok(fr)) => fr,
        Ok(Err(_)) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Response channel dropped").into_response()
        }
        Err(_) => {
            state.0.pending_requests.remove(&request_id);
            return (StatusCode::GATEWAY_TIMEOUT, "Request timed out").into_response()
        }
    };

    // 6. Decode and return the HTTP response
    let http_resp = match HttpResponsePayload::decode(response_frame.payload.as_slice()) {
        Ok(r) => r,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to decode response payload").into_response()
        }
    };

    let mut builder = axum::response::Response::builder().status(http_resp.status as u16);
    if let Some(h) = http_resp.headers {
        for (k, v) in h.headers {
            builder = builder.header(k, v);
        }
    }

    builder.body(Body::from(http_resp.body)).unwrap_or_else(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to construct internal response",
        )
            .into_response()
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    info!("Starting main-server initialization...");

    let state = SharedState(Arc::new(AppState {
        volunteers: DashMap::new(),
        pending_requests: DashMap::new(),
    }));

    // Start gRPC server
    let grpc_state = state.clone();
    let addr = "127.0.0.1:50051".parse()?;
    let grpc_server = ServerImpl { state: grpc_state };
    // Load TLS certificates
    let cert = std::fs::read_to_string("certs/server.crt").expect("Missing certs/server.crt");
    let key = std::fs::read_to_string("certs/server.key").expect("Missing certs/server.key");
    let identity = tonic::transport::Identity::from_pem(cert, key);
    let tls_config = tonic::transport::ServerTlsConfig::new().identity(identity);

    let grpc_task = tokio::spawn(async move {
        info!("gRPC (TLS) listening on {}", addr);
        Server::builder()
            .tls_config(tls_config)
            .unwrap()
            .add_service(TunnelServiceServer::new(grpc_server))
            .serve(addr)
            .await
            .unwrap();
    });

    // Start HTTP dispatch server
    let http_state = state.clone();
    let app = Router::new()
        .route("/dashboard", get(dashboard))
        .route("/api/volunteers", get(list_volunteers))
        .route("/*path", any(dispatch_request))
        .with_state(http_state);

    let http_addr: SocketAddr = "127.0.0.1:8080".parse()?;
    let listener = TcpListener::bind(http_addr).await?;
    info!("HTTP dispatch listening on {}", http_addr);
    
    let http_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Run Health Monitor loop
    let monitor_state = state.clone();
    let monitor_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let now = Instant::now();
            // Remove volunteers that haven't sent a heartbeat in 60 seconds
            monitor_state.0.volunteers.retain(|_, v| {
                if now.duration_since(v.last_heartbeat) > Duration::from_secs(60) {
                    warn!("Volunteer {} timeout. Removing from registry.", v.id);
                    false
                } else {
                    true
                }
            });
        }
    });

    let _ = tokio::join!(grpc_task, http_task, monitor_task);
    Ok(())
}
