mod churn;
mod dashboard;
mod db;
mod dispatch;
mod enrollment;
mod metrics_handler;
mod services;
mod state;

use axum::{
    routing::{delete, get, post},
    Router,
};
use chrono::Utc;
use metrics_exporter_prometheus::PrometheusBuilder;
use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "registrar=debug,info".into()),
        )
        .init();

    let db_path = std::env::var("OPENSHARD_DB")
        .unwrap_or_else(|_| "/opt/openshard/data/openshard.db".to_string());

    let database = db::Db::init(&db_path).await.unwrap_or_else(|e| {
        tracing::error!(error = %e, path = %db_path, "Failed to initialize database");
        std::process::exit(1);
    });

    let metrics_handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("Failed to install Prometheus recorder");

    let state = AppState::new(database, metrics_handle);

    // Crash recovery: re-populate in-memory state from the last known DB snapshot
    {
        let survivors = state.db.load_all().await;
        let count = survivors.len();
        if count > 0 {
            // Collect addr info before taking the lock so we don't hold it across awaits
            let addrs: Vec<(uuid::Uuid, String)> = survivors
                .iter()
                .map(|v| (v.id, v.info.service_addr.clone()))
                .collect();

            {
                let mut map = state.volunteers.write().await;
                for mut v in survivors {
                    // Grace period: reset heartbeat so churn doesn't evict before first heartbeat
                    v.last_heartbeat = Utc::now();
                    map.insert(v.id, v);
                }
            }

            for (id, addr) in addrs {
                // Best-effort: HAProxy may already know this server if it didn't restart
                if let Err(e) = haproxy_manager::add_volunteer("volunteers", id, &addr, 1).await {
                    tracing::debug!(volunteer_id = %id, error = %e, "HAProxy add on recovery (may already exist)");
                }
            }

            metrics::gauge!("openshard_volunteers_active").set(count as f64);
            tracing::info!(count, "Recovered volunteers from database");
        }
    }

    tokio::spawn(churn::run_churn_monitor(state.clone()));

    let app = Router::new()
        .route("/", get(dashboard::dashboard))
        .route("/enroll", post(enrollment::enroll))
        .route("/heartbeat", post(enrollment::heartbeat))
        .route("/enroll/:id", delete(enrollment::disconnect))
        .route("/volunteers", get(enrollment::list_volunteers))
        .route("/dispatch", post(dispatch::dispatch))
        .route("/services", post(services::register_service))
        .route("/services", get(services::list_services))
        .route("/metrics", get(metrics_handler::metrics_handler))
        .with_state(state);

    let addr = "0.0.0.0:3000";
    tracing::info!("Registrar listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
