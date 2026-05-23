use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::{AppState, HandshakeInfo, Metrics, VolunteerState, HEARTBEAT_INTERVAL_SECS};

#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    #[serde(flatten)]
    pub info: HandshakeInfo,
}

#[derive(Debug, Serialize)]
pub struct EnrollResponse {
    pub volunteer_id: String,
    pub heartbeat_interval_seconds: u64,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatRequest {
    pub volunteer_id: Uuid,
    pub cpu_pct: f32,
    pub mem_pct: f32,
    pub load_avg: f32,
    pub active_requests: u32,
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

    let volunteer = VolunteerState {
        id,
        metrics: Metrics {
            cpu_pct: 0.0,
            mem_pct: 0.0,
            load_avg: 0.0,
            active_requests: 0,
        },
        enrolled_at: now,
        last_heartbeat: now,
        info: body.info.clone(),
    };

    state.db.upsert_volunteer(&volunteer).await;

    let active_count = {
        let mut map = state.volunteers.write().await;
        map.insert(id, volunteer);
        map.len()
    };

    if let Err(e) = haproxy_manager::add_volunteer("volunteers", id, &body.info.service_addr, 100).await {
        tracing::warn!(volunteer_id = %id, error = %e, "Failed to add volunteer to HAProxy");
    }

    metrics::counter!("openshard_enrollments_total").increment(1);
    metrics::gauge!("openshard_volunteers_active").set(active_count as f64);

    tracing::info!(
        volunteer_id = %id,
        hostname = %body.info.hostname,
        addr = %body.info.service_addr,
        "Volunteer enrolled"
    );

    (
        StatusCode::CREATED,
        Json(EnrollResponse {
            volunteer_id: id.to_string(),
            heartbeat_interval_seconds: HEARTBEAT_INTERVAL_SECS,
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
                tracing::warn!(volunteer_id = %id, error = %e, "Failed to update HAProxy weight");
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
        Err(_) => return (
            StatusCode::BAD_REQUEST,
            err("Invalid UUID", "INVALID_ID").into_response(),
        ),
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
