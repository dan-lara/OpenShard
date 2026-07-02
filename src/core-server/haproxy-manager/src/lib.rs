pub mod config;
pub mod runtime;
pub mod subdomain;

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum Error {
    #[error("runtime API error: {0}")]
    Runtime(#[from] runtime::RuntimeError),
    #[error("config error: {0}")]
    Config(#[from] config::ConfigError),
    #[error("subdomain error: {0}")]
    Subdomain(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Register a new volunteer server into the given HAProxy backend.
///
/// Uses the runtime API — no config reload needed.
pub async fn add_volunteer(
    service_name: &str,
    volunteer_id: Uuid,
    addr: &str,
    weight: u32,
) -> Result<()> {
    let server_name = server_name_for(volunteer_id);
    runtime::add_server(service_name, &server_name, addr).await?;
    runtime::set_weight(service_name, &server_name, weight).await?;
    runtime::set_state(service_name, &server_name, "ready").await?;
    tracing::info!(
        service_name,
        volunteer_id = %volunteer_id,
        addr,
        weight,
        "Volunteer added to HAProxy backend"
    );
    Ok(())
}

/// Remove a volunteer server from the given HAProxy backend.
///
/// Sets the server to maintenance mode first, then deletes it via the runtime API.
pub async fn remove_volunteer(service_name: &str, volunteer_id: Uuid) -> Result<()> {
    let server_name = server_name_for(volunteer_id);
    runtime::set_state(service_name, &server_name, "maint").await?;
    runtime::remove_server(service_name, &server_name).await?;
    tracing::info!(
        service_name,
        volunteer_id = %volunteer_id,
        "Volunteer removed from HAProxy backend"
    );
    Ok(())
}

/// Add or update a volunteer server in the given backend (idempotent across re-enrolls).
///
/// A volunteer keeps its UUID across restarts (stable identity), but its tunnel
/// address changes every time the tunnel client reconnects. `add server` is
/// rejected when the server already exists, so we update via `set addr` first
/// and only fall back to `add server` when HAProxy reports "No such server".
pub async fn upsert_volunteer(
    service_name: &str,
    volunteer_id: Uuid,
    addr: &str,
    weight: u32,
) -> Result<()> {
    let server_name = server_name_for(volunteer_id);
    upsert_server(service_name, &server_name, addr, weight).await?;
    tracing::info!(
        service_name,
        volunteer_id = %volunteer_id,
        addr,
        weight,
        "Volunteer upserted in HAProxy backend"
    );
    Ok(())
}

/// Update the weight of a volunteer server in the given HAProxy backend.
///
/// Uses the runtime API — no config reload needed.
pub async fn set_weight(service_name: &str, volunteer_id: Uuid, weight: u32) -> Result<()> {
    let server_name = server_name_for(volunteer_id);
    runtime::set_weight(service_name, &server_name, weight).await?;
    tracing::debug!(
        service_name,
        volunteer_id = %volunteer_id,
        weight,
        "HAProxy weight updated"
    );
    Ok(())
}

/// Ensure a service backend's config (ACL + backend block) exists in haproxy.cfg.
///
/// Idempotent — safe to call on every startup. When the service is new this
/// writes the config and sends SIGUSR2; when it already exists it returns
/// immediately without a reload.  Callers that drive crash recovery should
/// call this for all services first, sleep ~1 s, then call
/// `restore_service_server` for each volunteer so that all server additions
/// happen after the final reload.
pub async fn ensure_service_config(service_name: &str, domain: &str) -> Result<()> {
    let backend = format!("svc_{}", service_name);
    config::add_service(&backend, domain).await?;
    Ok(())
}

/// Re-add a dynamic server entry to a service backend after a crash/restart.
///
/// Uses the fixed name "primary" so repeated calls replace rather than
/// accumulate. Unlike `assign_service`, this skips the config step (the caller
/// must call `ensure_service_config` first) so no SIGUSR2 reload is triggered.
pub async fn restore_service_server(
    service_name: &str,
    _volunteer_id: Uuid,
    tunnel_addr: &str,
) -> Result<()> {
    upsert_svc_server(service_name, tunnel_addr).await
}

/// Assign a service to a specific volunteer.
///
/// Creates the `svc_<service_name>` backend + frontend ACLs via the config module
/// (requires HAProxy reload), then upserts the fixed "primary" server pointing at
/// `tunnel_addr` via the runtime API. Using a fixed name prevents server accumulation
/// across restart cycles when tunnel ports are reused.
pub async fn assign_service(
    service_name: &str,
    domain: &str,
    volunteer_id: Uuid,
    tunnel_addr: &str,
) -> Result<()> {
    config::add_service(&format!("svc_{}", service_name), domain).await?;
    // Give HAProxy's new worker time to finish starting after the SIGUSR2 reload
    // before issuing runtime API commands against the new backend.
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    upsert_svc_server(service_name, tunnel_addr).await?;

    tracing::info!(
        service_name,
        domain,
        volunteer_id = %volunteer_id,
        tunnel_addr,
        "Service assigned to volunteer"
    );
    Ok(())
}

/// Add a new service backend + frontend routing rule to haproxy.cfg and reload.
pub async fn register_service(service_name: &str, domain: &str) -> Result<()> {
    config::add_service(service_name, domain).await?;
    tracing::info!(service_name, domain, "Service registered");
    Ok(())
}

/// Ensure a service backend exists, registering it if it does not.
///
/// Idempotent: safe to call on every startup or re-enrollment without
/// corrupting the config.
pub async fn ensure_service(service_name: &str, domain: &str) -> Result<()> {
    config::add_service(service_name, domain).await?;
    tracing::info!(service_name, domain, "Service ensured");
    Ok(())
}

/// Remove a service backend + frontend routing rule from haproxy.cfg and reload.
pub async fn unregister_service(service_name: &str) -> Result<()> {
    config::remove_service(service_name).await?;
    tracing::info!(service_name, "Service unregistered");
    Ok(())
}

/// List the names of all service backends currently present in haproxy.cfg.
pub async fn list_services() -> Result<Vec<String>> {
    Ok(config::list_services().await?)
}

/// Update the tunnel address of an existing volunteer in HAProxy.
///
/// Puts the server into maintenance, changes its address, then brings it back
/// to ready so health checks restart against the new endpoint. Applies to both
/// the `volunteers` backend and, when the volunteer has an assigned service, to
/// the corresponding `svc_*` backend as well.
///
/// `assigned_service` is `Some((service_name, new_tunnel_addr))` when the
/// volunteer is currently serving a named service.
pub async fn update_volunteer_addr(
    volunteer_id: Uuid,
    new_volunteers_addr: &str,
    assigned_service: Option<(String, String)>,
) -> Result<()> {
    let server_name = server_name_for(volunteer_id);

    runtime::set_state("volunteers", &server_name, "maint").await?;
    runtime::set_addr("volunteers", &server_name, new_volunteers_addr).await?;
    runtime::set_state("volunteers", &server_name, "ready").await?;

    if let Some((service_name, tunnel_addr)) = assigned_service {
        let backend = format!("svc_{}", service_name);
        runtime::set_state(&backend, "primary", "maint").await?;
        runtime::set_addr(&backend, "primary", &tunnel_addr).await?;
        runtime::set_state(&backend, "primary", "ready").await?;
    }

    tracing::info!(
        volunteer_id = %volunteer_id,
        new_volunteers_addr,
        "Volunteer address updated in HAProxy"
    );
    Ok(())
}

/// Upsert the fixed "primary" server in a svc backend.
///
/// Uses a fixed name so repeated assignments replace rather than accumulate.
async fn upsert_svc_server(service_name: &str, tunnel_addr: &str) -> Result<()> {
    let backend = format!("svc_{}", service_name);
    upsert_server(&backend, "primary", tunnel_addr, 100).await
}

/// Add or update a dynamic server, leaving it in the "ready" state.
///
/// `set addr` updates an existing server but fails with "No such server" when it
/// is absent; `add server` is silently rejected when the server already exists.
/// So we try `set addr` first and fall back to `add server` — correct in both
/// directions and idempotent across re-enrollments.
async fn upsert_server(
    backend: &str,
    server_name: &str,
    addr: &str,
    weight: u32,
) -> Result<()> {
    match runtime::set_addr(backend, server_name, addr).await {
        Ok(()) => {}
        Err(e) if e.to_string().contains("No such server") => {
            runtime::add_server(backend, server_name, addr).await?;
        }
        Err(e) => return Err(e.into()),
    }
    runtime::set_weight(backend, server_name, weight).await?;
    runtime::set_state(backend, server_name, "ready").await?;
    Ok(())
}

/// Derive a stable HAProxy server name from a UUID (hyphens not allowed in server names).
fn server_name_for(id: Uuid) -> String {
    id.simple().to_string()
}
