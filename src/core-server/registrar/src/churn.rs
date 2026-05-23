use std::time::Duration;
use tokio::time;

use crate::state::AppState;

const CHECK_INTERVAL_SECS: u64 = 15;

pub async fn run_churn_monitor(state: AppState) {
    let mut interval = time::interval(Duration::from_secs(CHECK_INTERVAL_SECS));

    loop {
        interval.tick().await;

        let dead: Vec<_> = {
            let map = state.volunteers.read().await;
            map.values()
                .filter(|v| !v.is_alive())
                .map(|v| (v.id, v.info.hostname.clone()))
                .collect()
        };

        if dead.is_empty() {
            continue;
        }

        for (id, hostname) in &dead {
            {
                let mut map = state.volunteers.write().await;
                map.remove(id);
            }

            state.db.remove_volunteer(*id).await;

            if let Err(e) = haproxy_manager::remove_volunteer("volunteers", *id).await {
                tracing::warn!(volunteer_id = %id, error = %e, "Failed to remove dead volunteer from HAProxy");
            }

            metrics::counter!("openshard_evictions_total").increment(1);
            tracing::info!(volunteer_id = %id, hostname = %hostname, "Volunteer removed by churn monitor");
        }

        let active_count = state.volunteers.read().await.len();
        metrics::gauge!("openshard_volunteers_active").set(active_count as f64);
    }
}
