use chrono::Utc;
use metrics_exporter_prometheus::PrometheusHandle;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::db::Db;

// Timeout: volunteer removed after 3 missed heartbeats (45s)
pub const HEARTBEAT_TIMEOUT_SECS: i64 = 45;
pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;

/// Static info sent once at enrollment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpu_cores: u32,
    pub cpu_model: String,
    pub memory_total_mb: u64,
    pub disk_free_gb: u64,
    pub docker_version: String,
    pub tunnel_version: String,
    /// IP:port where the volunteer's service is reachable via tunnel
    pub service_addr: String,
}

/// Live metrics updated on every heartbeat
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    pub cpu_pct: f32,
    pub mem_pct: f32,
    pub load_avg: f32,
    pub active_requests: u32,
}

impl Metrics {
    /// Dynamic weight for HAProxy: higher = more capable
    /// weight = 100 - (cpu*0.5 + mem*0.3 + active_requests*1.0 capped at 20), clamped to [1, 100]
    pub fn weight(&self) -> u32 {
        let req_pressure = (self.active_requests as f32).min(20.0);
        let score = 100.0 - (self.cpu_pct * 0.5 + self.mem_pct * 0.3 + req_pressure);
        score.clamp(1.0, 100.0) as u32
    }
}

/// A service that has been assigned to a volunteer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAssignment {
    pub service_name: String,
    pub domain: String,
    pub image: String,
    pub service_port: u16,
    pub assigned_at: chrono::DateTime<Utc>,
}

/// A service waiting to be assigned to an available volunteer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingService {
    pub name: String,
    pub domain: String,
    pub image: String,
    pub service_port: u16,
    pub registered_at: chrono::DateTime<Utc>,
}

/// Full state of a registered volunteer
#[derive(Debug, Clone, Serialize)]
pub struct VolunteerState {
    pub id: Uuid,
    pub info: HandshakeInfo,
    pub metrics: Metrics,
    pub enrolled_at: chrono::DateTime<Utc>,
    pub last_heartbeat: chrono::DateTime<Utc>,
    pub assigned_service: Option<ServiceAssignment>,
    pub service_running: Option<bool>,
    pub service_image: Option<String>,
}

impl VolunteerState {
    pub fn is_alive(&self) -> bool {
        let elapsed = Utc::now()
            .signed_duration_since(self.last_heartbeat)
            .num_seconds();
        elapsed < HEARTBEAT_TIMEOUT_SECS
    }
}

/// Shared application state — cloned cheaply via Arc
#[derive(Clone)]
pub struct AppState {
    pub volunteers: Arc<RwLock<HashMap<Uuid, VolunteerState>>>,
    pub pending_services: Arc<RwLock<VecDeque<PendingService>>>,
    pub db: Db,
    pub metrics: PrometheusHandle,
}

impl AppState {
    pub fn new(db: Db, metrics: PrometheusHandle) -> Self {
        Self {
            volunteers: Arc::new(RwLock::new(HashMap::new())),
            pending_services: Arc::new(RwLock::new(VecDeque::new())),
            db,
            metrics,
        }
    }

    /// Returns active volunteers sorted by weight descending (best first)
    pub async fn active_volunteers(&self) -> Vec<VolunteerState> {
        let map = self.volunteers.read().await;
        let mut active: Vec<VolunteerState> = map
            .values()
            .filter(|v| v.is_alive())
            .cloned()
            .collect();
        active.sort_by(|a, b| {
            b.metrics.weight().cmp(&a.metrics.weight())
        });
        active
    }
}