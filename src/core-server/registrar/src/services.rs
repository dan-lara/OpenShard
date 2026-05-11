use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct RegisterServiceRequest {
    pub name: String,
    pub domain: String,
}

#[derive(Debug, Serialize)]
pub struct RegisterServiceResponse {
    pub backend: String,
    pub domain: String,
}

/// POST /services
/// Registers a new service backend in HAProxy (idempotent).
pub async fn register_service(
    Json(body): Json<RegisterServiceRequest>,
) -> impl IntoResponse {
    let backend = format!("svc_{}", body.name);

    if let Err(e) = haproxy_manager::ensure_service(&backend, &body.domain).await {
        tracing::error!(name = %body.name, error = %e, "Failed to register service");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })).into_response(),
        );
    }

    tracing::info!(name = %body.name, backend = %backend, domain = %body.domain, "Service registered");
    (
        StatusCode::CREATED,
        Json(RegisterServiceResponse {
            backend,
            domain: body.domain,
        })
        .into_response(),
    )
}

/// GET /services
/// Returns the list of service backends currently registered in HAProxy.
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
