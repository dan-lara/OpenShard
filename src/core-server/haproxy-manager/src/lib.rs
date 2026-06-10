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

/// Assign a service to a specific volunteer.
///
/// Creates the `svc_<service_name>` backend + frontend ACLs via the config module
/// (requires HAProxy reload), then adds `vol-<volunteer_id>` pointing at `tunnel_addr`
/// via the runtime API.
pub async fn assign_service(
    service_name: &str,
    domain: &str,
    volunteer_id: Uuid,
    tunnel_addr: &str,
) -> Result<()> {
    let backend = format!("svc_{}", service_name);
    let server_name = server_name_for(volunteer_id);

    config::add_service(&backend, domain).await?;
    runtime::add_server(&backend, &server_name, tunnel_addr).await?;
    runtime::set_state(&backend, &server_name, "ready").await?;

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

/// Derive a stable HAProxy server name from a UUID (hyphens not allowed in server names).
fn server_name_for(id: Uuid) -> String {
    id.simple().to_string()
}
