use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::state::{AppState, PendingService, ServiceAssignment};

#[derive(Debug, Deserialize, ToSchema)]
pub struct RegisterServiceRequest {
    pub name: String,
    pub domain: String,
    pub image: String,
    pub port: u16,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RegisterServiceResponse {
    pub backend: String,
    pub domain: String,
    pub assigned_to: Option<String>,
}

/// POST /services
/// Registers a new service. Assigns it to the best free volunteer immediately,
/// or queues it as pending if none are available.
#[utoipa::path(
    post,
    path = "/services",
    tag = "services",
    request_body = RegisterServiceRequest,
    responses(
        (status = 201, description = "Service registered; assigned to a volunteer or queued as pending", body = RegisterServiceResponse),
        (status = 500, description = "HAProxy assignment failed")
    )
)]
pub async fn register_service(
    State(state): State<AppState>,
    Json(body): Json<RegisterServiceRequest>,
) -> impl IntoResponse {
    let free_volunteer = state
        .active_volunteers()
        .await
        .into_iter()
        .find(|v| v.assigned_service.is_none());

    if let Some(volunteer) = free_volunteer {
        let tunnel_addr = tunnel_addr_for(&volunteer.info.service_addr);

        if let Err(e) =
            haproxy_manager::assign_service(&body.name, &body.domain, volunteer.id, &tunnel_addr)
                .await
        {
            tracing::error!(name = %body.name, error = %e, "Failed to assign service via HAProxy");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })).into_response(),
            );
        }

        let assignment = ServiceAssignment {
            service_name: body.name.clone(),
            domain: body.domain.clone(),
            image: body.image.clone(),
            service_port: body.port,
            assigned_at: Utc::now(),
        };

        state.db.upsert_assignment(volunteer.id, &assignment).await;

        {
            let mut map = state.volunteers.write().await;
            if let Some(v) = map.get_mut(&volunteer.id) {
                v.assigned_service = Some(assignment);
            }
        }

        tracing::info!(
            name = %body.name,
            domain = %body.domain,
            volunteer_id = %volunteer.id,
            "Service assigned immediately to volunteer"
        );

        (
            StatusCode::CREATED,
            Json(RegisterServiceResponse {
                backend: format!("svc_{}", body.name),
                domain: body.domain,
                assigned_to: Some(volunteer.id.to_string()),
            })
            .into_response(),
        )
    } else {
        state.pending_services.write().await.push_back(PendingService {
            name: body.name.clone(),
            domain: body.domain.clone(),
            image: body.image.clone(),
            service_port: body.port,
            registered_at: Utc::now(),
        });

        tracing::info!(
            name = %body.name,
            domain = %body.domain,
            "No free volunteer available; service queued as pending"
        );

        (
            StatusCode::CREATED,
            Json(RegisterServiceResponse {
                backend: format!("svc_{}", body.name),
                domain: body.domain,
                assigned_to: None,
            })
            .into_response(),
        )
    }
}

/// GET /services
/// Returns the list of service backends currently registered in HAProxy.
#[utoipa::path(
    get,
    path = "/services",
    tag = "services",
    responses(
        (status = 200, description = "Service backends currently registered in HAProxy"),
        (status = 500, description = "Failed to read HAProxy config")
    )
)]
pub async fn list_services() -> impl IntoResponse {
    match haproxy_manager::list_services().await {
        Ok(services) => (
            StatusCode::OK,
            Json(serde_json::json!({ "services": services })).into_response(),
        ),
        Err(e) => {
            tracing::error!(error = %e, "Failed to list services");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })).into_response(),
            )
        }
    }
}

/// Returns the tunnel address HAProxy should use to reach this volunteer's service.
/// `service_addr` is already "tunnel_ip:port" as resolved from TUNNEL_HOST, so we
/// use it directly — no remapping to 127.0.0.1, which would break when HAProxy
/// runs inside a Docker container (different network namespace from the tunnel server).
fn tunnel_addr_for(service_addr: &str) -> String {
    service_addr.to_string()
}
