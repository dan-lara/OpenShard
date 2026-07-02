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

/// Block until HAProxy's admin socket appears (up to ~15s), so runtime API
/// calls made during crash recovery don't fail with "No such file or directory".
async fn wait_for_haproxy_socket() {
    let path = std::env::var("HAPROXY_SOCKET")
        .unwrap_or_else(|_| "/run/haproxy/admin.sock".to_string());
    for _ in 0..30 {
        if std::path::Path::new(&path).exists() {
            tracing::info!(path = %path, "HAProxy admin socket present");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    tracing::warn!(path = %path, "HAProxy admin socket not present after wait; proceeding anyway");
}

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

    // Drop assignment rows left behind by the pre-stable-identity behaviour
    // (a fresh UUID on every enroll orphaned every assignment).
    let pruned = state.db.prune_orphan_assignments().await;
    if pruned > 0 {
        tracing::info!(pruned, "Pruned orphaned service assignments");
    }

    // The HAProxy admin socket is created by HAProxy a moment after this process
    // starts (same container). Runtime API calls fail until it exists, so wait
    // for it before crash recovery touches the runtime API.
    wait_for_haproxy_socket().await;

    // Crash recovery: re-populate in-memory state from the last known DB snapshot.
    //
    // Two-pass approach so service config reloads (SIGUSR2) all happen in pass 1
    // before any runtime server additions in pass 2.  Mixing them would cause
    // each HAProxy reload to wipe the servers added in the previous iteration.
    {
        let survivors = state.db.load_all().await;
        let count = survivors.len();
        if count > 0 {
            // Collect recovery data before consuming survivors into the map.
            type SvcInfo = Option<(String, String)>; // (service_name, domain)
            let recovery: Vec<(uuid::Uuid, String, SvcInfo)> = survivors
                .iter()
                .map(|v| {
                    let svc = v.assigned_service.as_ref()
                        .map(|s| (s.service_name.clone(), s.domain.clone()));
                    (v.id, v.info.service_addr.clone(), svc)
                })
                .collect();

            {
                let mut map = state.volunteers.write().await;
                for mut v in survivors {
                    // Grace period: reset heartbeat so churn doesn't evict before first heartbeat
                    v.last_heartbeat = Utc::now();
                    map.insert(v.id, v);
                }
            }

            // Pass 1: restore service backend configs (may trigger SIGUSR2 reloads).
            let any_services = recovery.iter().any(|(_, _, s)| s.is_some());
            for (_, _, svc) in &recovery {
                if let Some((service_name, domain)) = svc {
                    if let Err(e) = haproxy_manager::ensure_service_config(service_name, domain).await {
                        tracing::warn!(service_name, error = %e, "Failed to restore service config on recovery");
                    }
                }
            }
            // Let the final HAProxy worker settle before adding runtime servers.
            if any_services {
                tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            }

            // Pass 2: add all runtime server entries (no more config changes / reloads).
            for (id, addr, svc) in &recovery {
                if let Err(e) = haproxy_manager::upsert_volunteer("volunteers", *id, addr, 1).await {
                    tracing::debug!(volunteer_id = %id, error = %e, "HAProxy upsert on recovery");
                }
                if let Some((service_name, _)) = svc {
                    if let Err(e) = haproxy_manager::restore_service_server(service_name, *id, addr).await {
                        tracing::debug!(volunteer_id = %id, service_name, error = %e, "HAProxy svc server restore on recovery");
                    }
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
        .route("/volunteers/:id/tunnel-port", post(enrollment::update_tunnel_port))
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
