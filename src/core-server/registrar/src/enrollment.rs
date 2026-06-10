// src/enrollment.rs
//
// The tunnel server now owns port allocation : it assigns a public port to
// each agent when the control connection is established, and sends it back
// as 2 bytes.  The registrar no longer needs to find a free port itself;
// it just records whatever the tunnel server reports.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::{AppState, HandshakeInfo, Metrics, ServiceAssignment, VolunteerState, HEARTBEAT_INTERVAL_SECS};

#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    #[serde(flatten)]
    pub info: HandshakeInfo,
    /// Public port assigned by the tunnel server for this agent.
    /// The client reads this from the tunnel server's 2-byte handshake
    /// response and includes it in the enroll payload.
    pub tunnel_public_port: u16,
}

#[derive(Debug, Serialize)]
pub struct AssignmentPayload {
    pub image: String,
    pub service_port: u16,
}

#[derive(Debug, Serialize)]
pub struct EnrollResponse {
    pub volunteer_id: String,
    pub heartbeat_interval_seconds: u64,
    pub assignment: Option<AssignmentPayload>,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatRequest {
    pub volunteer_id: Uuid,
    pub cpu_pct: f32,
    pub mem_pct: f32,
    pub load_avg: f32,
    pub active_requests: u32,
    #[serde(default)]
    pub service_running: Option<bool>,
    #[serde(default)]
    pub service_image: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: String,
}

fn err(msg: &str, code: &str) -> Json<ErrorResponse> {
    Json(ErrorResponse {
        error: msg.to_string(),
        code: code.to_string(),
    })
}

pub async fn enroll(
    State(state): State<AppState>,
    Json(body): Json<EnrollRequest>,
) -> impl IntoResponse {
    let id = Uuid::new_v4();
    let now = Utc::now();

    let tunnel_host = std::env::var("TUNNEL_HOST").unwrap_or_else(|_| "tunnel".to_string());
    
    // Resolve the hostname to an IP address because HAProxy's `add server` 
    // runtime API requires a strict IP and will fail to route to a hostname.
    let resolved_ip = match tokio::net::lookup_host(format!("{}:0", tunnel_host)).await {
        Ok(mut addrs) => {
            if let Some(addr) = addrs.next() {
                addr.ip().to_string()
            } else {
                tunnel_host.clone()
            }
        }
        Err(e) => {
            tracing::warn!("Failed to resolve TUNNEL_HOST {}: {}", tunnel_host, e);
            tunnel_host.clone()
        }
    };

    let service_addr = format!("{}:{}", resolved_ip, body.tunnel_public_port);
    let hostname = service_addr.clone();

    let mut info = body.info.clone();
    info.hostname = hostname.clone();
    info.service_addr = service_addr.clone();

    let mut volunteer = VolunteerState {
        id,
        metrics: Metrics {
            cpu_pct: 0.0,
            mem_pct: 0.0,
            load_avg: 0.0,
            active_requests: 0,
        },
        enrolled_at: now,
        last_heartbeat: now,
        info,
        assigned_service: None,
        service_running: None,
        service_image: None,
    };

    // Dequeue a pending service and assign it to this volunteer if one is available.
    let pending = {
        let mut queue = state.pending_services.write().await;
        queue.pop_front()
    };

    let assignment_payload = if let Some(pending) = pending {
        let tunnel_addr = format!("127.0.0.1:{}", body.tunnel_public_port);
        match haproxy_manager::assign_service(&pending.name, &pending.domain, id, &tunnel_addr).await {
            Ok(()) => {
                let assignment = ServiceAssignment {
                    service_name: pending.name.clone(),
                    domain: pending.domain.clone(),
                    image: pending.image.clone(),
                    service_port: pending.service_port,
                    assigned_at: now,
                };
                state.db.upsert_assignment(id, &assignment).await;
                let payload = AssignmentPayload {
                    image: assignment.image.clone(),
                    service_port: assignment.service_port,
                };
                volunteer.assigned_service = Some(assignment);
                tracing::info!(
                    volunteer_id = %id,
                    name = %pending.name,
                    domain = %pending.domain,
                    "Assigned pending service to newly enrolled volunteer"
                );
                Some(payload)
            }
            Err(e) => {
                tracing::error!(
                    volunteer_id = %id,
                    name = %pending.name,
                    error = %e,
                    "Failed to assign pending service via HAProxy; re-queuing"
                );
                state.pending_services.write().await.push_front(pending);
                None
            }
        }
    } else {
        None
    };

    state.db.upsert_volunteer(&volunteer).await;

    let active_count = {
        let mut map = state.volunteers.write().await;
        map.insert(id, volunteer.clone());
        map.len()
    };

    if let Err(e) = haproxy_manager::add_volunteer("volunteers", id, &service_addr, 100).await {
        tracing::warn!(volunteer_id = %id, error = %e, "Failed to add volunteer to HAProxy");
    }

    metrics::counter!("openshard_enrollments_total").increment(1);
    metrics::gauge!("openshard_volunteers_active").set(active_count as f64);

    tracing::info!(
        volunteer_id = %id,
        hostname = %hostname,
        addr = %service_addr,
        tunnel_public_port = body.tunnel_public_port,
        "Volunteer enrolled"
    );

    (
        StatusCode::CREATED,
        Json(EnrollResponse {
            volunteer_id: id.to_string(),
            heartbeat_interval_seconds: HEARTBEAT_INTERVAL_SECS,
            assignment: assignment_payload,
        }),
    )
}

pub async fn heartbeat(
    State(state): State<AppState>,
    Json(body): Json<HeartbeatRequest>,
) -> impl IntoResponse {
    let updated = {
        let mut map = state.volunteers.write().await;
        match map.get_mut(&body.volunteer_id) {
            None => None,
            Some(volunteer) => {
                let m = Metrics {
                    cpu_pct: body.cpu_pct,
                    mem_pct: body.mem_pct,
                    load_avg: body.load_avg,
                    active_requests: body.active_requests,
                };
                let weight = m.weight();
                volunteer.metrics = m;
                volunteer.last_heartbeat = Utc::now();
                if body.service_running.is_some() {
                    volunteer.service_running = body.service_running;
                }
                if body.service_image.is_some() {
                    volunteer.service_image = body.service_image.clone();
                }
                Some((volunteer.clone(), weight))
            }
        }
    };

    match updated {
        None => (
            StatusCode::NOT_FOUND,
            err("Volunteer not found", "VOLUNTEER_NOT_FOUND").into_response(),
        ),
        Some((volunteer, weight)) => {
            let id = volunteer.id;
            let hostname = volunteer.info.hostname.clone();

            state.db.upsert_volunteer(&volunteer).await;

            if let Err(e) = haproxy_manager::set_weight("volunteers", id, weight).await {
                let msg = e.to_string();
                if msg.contains("No such server") {
                    // HAProxy lost state (e.g. after restart) : re-add then set weight
                    if let Err(e2) = haproxy_manager::add_volunteer("volunteers", id, &volunteer.info.service_addr, weight).await {
                        tracing::warn!(volunteer_id = %id, error = %e2, "Failed to re-add volunteer to HAProxy after missing server");
                    }
                } else {
                    tracing::warn!(volunteer_id = %id, error = %e, "Failed to update HAProxy weight");
                }
            }

            metrics::counter!("openshard_heartbeats_total").increment(1);
            metrics::gauge!("openshard_volunteer_weight", "hostname" => hostname.clone()).set(weight as f64);
            metrics::gauge!("openshard_volunteer_cpu_pct", "hostname" => hostname.clone()).set(body.cpu_pct as f64);
            metrics::gauge!("openshard_volunteer_mem_pct", "hostname" => hostname).set(body.mem_pct as f64);

            (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })).into_response())
        }
    }
}

pub async fn disconnect(
    State(state): State<AppState>,
    Path(id_str): Path<String>,
) -> impl IntoResponse {
    let id = match Uuid::parse_str(&id_str) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                err("Invalid UUID", "INVALID_ID").into_response(),
            )
        }
    };

    let (removed, active_count) = {
        let mut map = state.volunteers.write().await;
        let v = map.remove(&id);
        let count = map.len();
        (v, count)
    };

    match removed {
        None => (
            StatusCode::NOT_FOUND,
            err("Volunteer not found", "VOLUNTEER_NOT_FOUND").into_response(),
        ),
        Some(v) => {
            state.db.remove_volunteer(id).await;

            if let Err(e) = haproxy_manager::remove_volunteer("volunteers", id).await {
                tracing::warn!(volunteer_id = %id, error = %e, "Failed to remove from HAProxy");
            }

            metrics::gauge!("openshard_volunteers_active").set(active_count as f64);

            tracing::info!(volunteer_id = %id, hostname = %v.info.hostname, "Volunteer disconnected");
            (
                StatusCode::OK,
                Json(serde_json::json!({ "status": "disconnected" })).into_response(),
            )
        }
    }
}

pub async fn list_volunteers(State(state): State<AppState>) -> impl IntoResponse {
    let active = state.active_volunteers().await;
    Json(active)
}