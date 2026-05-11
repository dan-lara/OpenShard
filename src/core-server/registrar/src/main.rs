mod churn;
mod dispatch;
mod enrollment;
mod services;
mod state;

use axum::{
    routing::{delete, get, post},
    Router,
};
use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "registrar=debug,info".into()),
        )
        .init();

    let state = AppState::new();

    // Start churn monitor as a background task
    tokio::spawn(churn::run_churn_monitor(state.clone()));

    let app = Router::new()
        .route("/enroll", post(enrollment::enroll))
        .route("/heartbeat", post(enrollment::heartbeat))
        .route("/enroll/:id", delete(enrollment::disconnect))
        .route("/volunteers", get(enrollment::list_volunteers))
        // Temporary dispatch endpoint — replaced by HAProxy routing later
        .route("/dispatch", post(dispatch::dispatch))
        .route("/services", post(services::register_service))
        .route("/services", get(services::list_services))
        .with_state(state);

    let addr = "0.0.0.0:3000";
    tracing::info!("Registrar listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}