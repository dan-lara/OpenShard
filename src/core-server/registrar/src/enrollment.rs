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

    {
        let mut map = state.volunteers.write().await;
        map.insert(id, volunteer);
    }

    if let Err(e) = haproxy_manager::add_volunteer("volunteers", id, &body.info.service_addr, 100).await {
        tracing::warn!(volunteer_id = %id, error = %e, "Failed to add volunteer to HAProxy");
    }

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
    let mut map = state.volunteers.write().await;

    match map.get_mut(&body.volunteer_id) {
        None => (
            StatusCode::NOT_FOUND,
            err("Volunteer not found", "VOLUNTEER_NOT_FOUND").into_response(),
        ),
        Some(volunteer) => {
            let metrics = Metrics {
                cpu_pct: body.cpu_pct,
                mem_pct: body.mem_pct,
                load_avg: body.load_avg,
                active_requests: body.active_requests,
            };
            let weight = metrics.weight();
            volunteer.metrics = metrics;
            volunteer.last_heartbeat = Utc::now();
            let id = volunteer.id;
            drop(map);

            if let Err(e) = haproxy_manager::set_weight("volunteers", id, weight).await {
                tracing::warn!(volunteer_id = %id, error = %e, "Failed to update HAProxy weight");
            }

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

    let removed = {
        let mut map = state.volunteers.write().await;
        map.remove(&id)
    };

    match removed {
        None => (
            StatusCode::NOT_FOUND,
            err("Volunteer not found", "VOLUNTEER_NOT_FOUND").into_response(),
        ),
        Some(v) => {
            if let Err(e) = haproxy_manager::remove_volunteer("volunteers", id).await {
                tracing::warn!(volunteer_id = %id, error = %e, "Failed to remove from HAProxy");
            }
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