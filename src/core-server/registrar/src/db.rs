use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use std::str::FromStr;
use uuid::Uuid;

use crate::state::{HandshakeInfo, Metrics, VolunteerState};

#[derive(Clone)]
pub struct Db(SqlitePool);

const CREATE_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS volunteers (
    id               TEXT PRIMARY KEY,
    hostname         TEXT NOT NULL,
    os               TEXT NOT NULL,
    arch             TEXT NOT NULL,
    cpu_cores        INTEGER NOT NULL,
    cpu_model        TEXT NOT NULL,
    memory_total_mb  INTEGER NOT NULL,
    disk_free_gb     INTEGER NOT NULL,
    docker_version   TEXT NOT NULL,
    tunnel_version   TEXT NOT NULL,
    service_addr     TEXT NOT NULL,
    cpu_pct          REAL NOT NULL DEFAULT 0.0,
    mem_pct          REAL NOT NULL DEFAULT 0.0,
    load_avg         REAL NOT NULL DEFAULT 0.0,
    active_requests  INTEGER NOT NULL DEFAULT 0,
    enrolled_at      TEXT NOT NULL,
    last_heartbeat   TEXT NOT NULL
)
"#;

#[derive(sqlx::FromRow)]
struct DbRow {
    id: String,
    hostname: String,
    os: String,
    arch: String,
    cpu_cores: i64,
    cpu_model: String,
    memory_total_mb: i64,
    disk_free_gb: i64,
    docker_version: String,
    tunnel_version: String,
    service_addr: String,
    cpu_pct: f64,
    mem_pct: f64,
    load_avg: f64,
    active_requests: i64,
    enrolled_at: String,
    last_heartbeat: String,
}

impl TryFrom<DbRow> for VolunteerState {
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn try_from(r: DbRow) -> Result<Self, Self::Error> {
        Ok(VolunteerState {
            id: Uuid::parse_str(&r.id)?,
            enrolled_at: r.enrolled_at.parse()?,
            last_heartbeat: r.last_heartbeat.parse()?,
            info: HandshakeInfo {
                hostname: r.hostname,
                os: r.os,
                arch: r.arch,
                cpu_cores: r.cpu_cores as u32,
                cpu_model: r.cpu_model,
                memory_total_mb: r.memory_total_mb as u64,
                disk_free_gb: r.disk_free_gb as u64,
                docker_version: r.docker_version,
                tunnel_version: r.tunnel_version,
                service_addr: r.service_addr,
            },
            metrics: Metrics {
                cpu_pct: r.cpu_pct as f32,
                mem_pct: r.mem_pct as f32,
                load_avg: r.load_avg as f32,
                active_requests: r.active_requests as u32,
            },
            assigned_service: None,
        })
    }
}

impl Db {
    pub async fn init(path: &str) -> Result<Self, sqlx::Error> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}", path))?
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(opts).await?;
        sqlx::query(CREATE_TABLE).execute(&pool).await?;
        Ok(Self(pool))
    }

    pub async fn upsert_volunteer(&self, v: &VolunteerState) {
        let result = sqlx::query(
            "INSERT OR REPLACE INTO volunteers
             (id, hostname, os, arch, cpu_cores, cpu_model, memory_total_mb, disk_free_gb,
              docker_version, tunnel_version, service_addr,
              cpu_pct, mem_pct, load_avg, active_requests, enrolled_at, last_heartbeat)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        )
        .bind(v.id.to_string())
        .bind(&v.info.hostname)
        .bind(&v.info.os)
        .bind(&v.info.arch)
        .bind(v.info.cpu_cores as i64)
        .bind(&v.info.cpu_model)
        .bind(v.info.memory_total_mb as i64)
        .bind(v.info.disk_free_gb as i64)
        .bind(&v.info.docker_version)
        .bind(&v.info.tunnel_version)
        .bind(&v.info.service_addr)
        .bind(v.metrics.cpu_pct as f64)
        .bind(v.metrics.mem_pct as f64)
        .bind(v.metrics.load_avg as f64)
        .bind(v.metrics.active_requests as i64)
        .bind(v.enrolled_at.to_rfc3339())
        .bind(v.last_heartbeat.to_rfc3339())
        .execute(&self.0)
        .await;

        if let Err(e) = result {
            tracing::warn!(error = %e, "Failed to upsert volunteer to DB");
        }
    }

    pub async fn remove_volunteer(&self, id: Uuid) {
        if let Err(e) = sqlx::query("DELETE FROM volunteers WHERE id = ?1")
            .bind(id.to_string())
            .execute(&self.0)
            .await
        {
            tracing::warn!(error = %e, "Failed to remove volunteer from DB");
        }
    }

    pub async fn load_all(&self) -> Vec<VolunteerState> {
        match sqlx::query_as::<_, DbRow>("SELECT * FROM volunteers")
            .fetch_all(&self.0)
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|r| {
                    VolunteerState::try_from(r)
                        .map_err(|e| tracing::warn!(error = %e, "Skipping corrupt DB row"))
                        .ok()
                })
                .collect(),
            Err(e) => {
                tracing::error!(error = %e, "Failed to load volunteers from DB");
                vec![]
            }
        }
    }
}
