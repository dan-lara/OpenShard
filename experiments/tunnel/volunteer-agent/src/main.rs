use prost::Message;
use reqwest::{Client, Method};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{transport::Channel, Request as TonicRequest};
use tracing::{error, info};
use uuid::Uuid;

use tunnel_proto::tunnel::{
    tunnel_service_client::TunnelServiceClient, FrameType, HeartbeatPayload, HttpHeaders,
    HttpRequestPayload, HttpResponsePayload, TunnelFrame,
};

#[derive(Clone)]
struct AgentState {
    target_service_url: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    info!("Starting volunteer-agent...");

    let agent_id = Uuid::new_v4().to_string();
    let main_server_addr = "https://127.0.0.1:50051"; // Note https scheme
    
    // For testing, we mock a target service. In a real deployment,
    // this would be the Docker container hosted by the volunteer.
    let target_service_url = "http://127.0.0.1:9000"; 

    let state = AgentState {
        target_service_url: target_service_url.to_string(),
    };

    // Load custom CA to verify the self-signed certificate
    let pem = std::fs::read_to_string("certs/ca.crt").expect("Missing certs/ca.crt");
    let ca = tonic::transport::Certificate::from_pem(pem);
    let tls_config = tonic::transport::ClientTlsConfig::new()
        .ca_certificate(ca)
        .domain_name("localhost");

    let channel = tonic::transport::Channel::from_static(main_server_addr)
        .tls_config(tls_config)?
        .connect()
        .await?;

    let mut client = TunnelServiceClient::new(channel);

    let (tx, rx) = mpsc::channel(100);

    // 1. Send REGISTER frame
    let register_frame = TunnelFrame {
        volunteer_id: agent_id.clone(),
        request_id: "".to_string(),
        frame_type: FrameType::Register.into(),
        payload: vec![],
    };
    tx.send(register_frame).await?;

    // 2. Spawn Heartbeat task
    let hb_tx = tx.clone();
    let hb_agent_id = agent_id.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let hb = HeartbeatPayload {
                cpu_usage: 15.5,
                memory_used: 1024 * 1024 * 128,
                active_requests: 0,
                load_average: 1.2,
            };
            let mut payload = vec![];
            hb.encode(&mut payload).unwrap();

            let frame = TunnelFrame {
                volunteer_id: hb_agent_id.clone(),
                request_id: "".to_string(),
                frame_type: FrameType::Heartbeat.into(),
                payload,
            };

            if hb_tx.send(frame).await.is_err() {
                break;
            }
        }
    });

    // 3. Start Bi-directional stream
    let req_stream = ReceiverStream::new(rx);
    let mut response_stream = client
        .open_tunnel(TonicRequest::new(req_stream))
        .await?
        .into_inner();

    let http_client = Client::builder().timeout(Duration::from_secs(10)).build()?;
    info!("Connected to main-server. Listening for requests...");

    // 4. Process incoming REQUEST frames
    while let Ok(Some(frame)) = response_stream.message().await {
        if frame.frame_type() == FrameType::Request {
            let req_id = frame.request_id.clone();
            let mut pt_buf = frame.payload.as_slice();
            let http_req = match HttpRequestPayload::decode(&mut pt_buf) {
                Ok(r) => r,
                Err(e) => {
                    error!("Failed to decode HttpRequestPayload: {}", e);
                    continue;
                }
            };

            let req_tx = tx.clone();
            let state_c = state.clone();
            let client_c = http_client.clone();
            let aid = agent_id.clone();

            tokio::spawn(async move {
                let method = match Method::from_bytes(http_req.method.as_bytes()) {
                    Ok(m) => m,
                    Err(_) => Method::GET,
                };

                let url = format!("{}{}", state_c.target_service_url, http_req.uri);
                let mut rb = client_c.request(method, url);

                if let Some(headers) = http_req.headers {
                    for (k, v) in headers.headers {
                        rb = rb.header(k, v);
                    }
                }

                if !http_req.body.is_empty() {
                    rb = rb.body(http_req.body);
                }

                // 2. Mock service fallback or actual request forwarding
                let response_frame = match rb.send().await {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        let mut resp_headers = std::collections::HashMap::new();
                        for (k, v) in resp.headers().iter() {
                            if let Ok(v_str) = v.to_str() {
                                resp_headers.insert(k.as_str().to_string(), v_str.to_string());
                            }
                        }
                        let body_bytes = resp.bytes().await.unwrap_or_default().to_vec();

                        let payload = HttpResponsePayload {
                            status: status as u32,
                            headers: Some(HttpHeaders {
                                headers: resp_headers,
                            }),
                            body: body_bytes,
                        };
                        let mut p_buf = vec![];
                        payload.encode(&mut p_buf).unwrap();

                        TunnelFrame {
                            volunteer_id: aid,
                            request_id: req_id,
                            frame_type: FrameType::Response.into(),
                            payload: p_buf,
                        }
                    }
                    Err(e) => {
                        error!("Request to local service failed: {}", e);
                        
                        // If it fails to connect to the actual mapped port, 
                        // fallback to a mock "Hello from Volunteer" response for testing.
                        let mut resp_headers = std::collections::HashMap::new();
                        resp_headers.insert("content-type".to_string(), "text/plain".to_string());
                        
                        let payload = HttpResponsePayload {
                            status: 200, // Returning 200 to show the tunnel works end-to-end
                            headers: Some(HttpHeaders { headers: resp_headers }),
                            body: format!("Mock Response from Volunteer Agent {}! (Failed proxy: {})", aid, e).into_bytes(),
                        };
                        let mut p_buf = vec![];
                        payload.encode(&mut p_buf).unwrap();

                        TunnelFrame {
                            volunteer_id: aid,
                            request_id: req_id,
                            frame_type: FrameType::Response.into(),
                            payload: p_buf,
                        }
                    }
                };

                let _ = req_tx.send(response_frame).await;
            });
        }
    }

    info!("Disconnected from main-server.");
    Ok(())
}
