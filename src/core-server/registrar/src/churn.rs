use chrono::Utc;
use std::time::Duration;
use tokio::time;

use crate::state::{AppState, PendingService, ServiceAssignment};

const CHECK_INTERVAL_SECS: u64 = 15;

pub async fn run_churn_monitor(state: AppState) {
    let mut interval = time::interval(Duration::from_secs(CHECK_INTERVAL_SECS));

    loop {
        interval.tick().await;

        let dead: Vec<(uuid::Uuid, String, Option<ServiceAssignment>)> = {
            let map = state.volunteers.read().await;
            map.values()
                .filter(|v| !v.is_alive())
                .map(|v| (v.id, v.info.hostname.clone(), v.assigned_service.clone()))
                .collect()
        };

        if dead.is_empty() {
            continue;
        }

        for (id, hostname, assignment) in &dead {
            {
                let mut map = state.volunteers.write().await;
                map.remove(id);
            }

            state.db.remove_volunteer(*id).await;

            if let Err(e) = haproxy_manager::remove_volunteer("volunteers", *id).await {
                tracing::warn!(volunteer_id = %id, error = %e, "Failed to remove dead volunteer from HAProxy");
            }

            // Re-queue the service so the next enrolling (or returning) volunteer
            // picks it up, and drop the now-orphaned assignment row.
            if let Some(a) = assignment {
                state.db.remove_assignment(*id).await;
                state.pending_services.write().await.push_back(PendingService {
                    name: a.service_name.clone(),
                    domain: a.domain.clone(),
                    image: a.image.clone(),
                    service_port: a.service_port,
                    registered_at: Utc::now(),
                });
                tracing::info!(
                    volunteer_id = %id,
                    name = %a.service_name,
                    "Re-queued service from churned volunteer"
                );
            }

            metrics::counter!("openshard_evictions_total").increment(1);
            tracing::info!(volunteer_id = %id, hostname = %hostname, "Volunteer removed by churn monitor");
        }

        let active_count = state.volunteers.read().await.len();
        metrics::gauge!("openshard_volunteers_active").set(active_count as f64);
    }
}
