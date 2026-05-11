use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use reqwest::Client;

use crate::state::AppState;

/// POST /dispatch (internal) or used as a catch-all proxy handler.
///
/// Picks the volunteer with the highest weight (lowest load) and
/// forwards the request. Falls back to 503 if no volunteers are available.
///
/// NOTE: This is a temporary direct HTTP forward.
/// Once the tunnel (Module A) is ready, this will forward
/// through the tunnel instead of directly to service_addr.
pub async fn dispatch(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Response {
    let active = state.active_volunteers().await;

    let volunteer = match active.first() {
        Some(v) => v.clone(),
        None => {
            tracing::warn!("Dispatch failed: no active volunteers");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "No volunteers available",
            )
                .into_response();
        }
    };

    let target_addr = &volunteer.info.service_addr;
    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let url = format!("http://{}{}", target_addr, path);

    tracing::info!(
        volunteer_id = %volunteer.id,
        hostname = %volunteer.info.hostname,
        weight = volunteer.metrics.weight(),
        target = %url,
        "Dispatching request"
    );

    let client = Client::new();

    let method = reqwest::Method::from_bytes(req.method().as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);

    let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "Failed to read request body");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let result = client
        .request(method, &url)
        .body(body_bytes)
        .send()
        .await;

    match result {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = resp.bytes().await.unwrap_or_default();
            (status, body).into_response()
        }
        Err(e) => {
            tracing::error!(
                volunteer_id = %volunteer.id,
                error = %e,
                "Dispatch request failed"
            );
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}