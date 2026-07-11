use std::env;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("haproxy returned an error response: {0}")]
    Response(String),
    #[error("unix sockets are not supported on this platform")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, RuntimeError>;

#[allow(dead_code)]
fn socket_path() -> String {
    env::var("HAPROXY_SOCKET").unwrap_or_else(|_| "/run/haproxy/admin.sock".to_string())
}

/// Send a single command to the HAProxy runtime API and return the response.
#[cfg(unix)]
pub async fn send_command(cmd: &str) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let path = socket_path();
    let mut stream = UnixStream::connect(&path).await?;

    let command = format!("{}\n", cmd);
    stream.write_all(command.as_bytes()).await?;
    // Signal we are done writing so HAProxy flushes its response.
    stream.shutdown().await?;

    let mut response = String::new();
    stream.read_to_string(&mut response).await?;

    let response = response.trim().to_string();
    const ERROR_PREFIXES: &[&str] = &[
        "Can't", "No such", "Unknown", "Invalid", "Alert", "error",
    ];
    if ERROR_PREFIXES.iter().any(|p| response.starts_with(p)) {
        return Err(RuntimeError::Response(response));
    }

    Ok(response)
}

#[cfg(not(unix))]
pub async fn send_command(_cmd: &str) -> Result<String> {
    Err(RuntimeError::Unsupported)
}

/// Add a dynamic server to a backend.
pub async fn add_server(backend: &str, server_name: &str, addr: &str) -> Result<()> {
    let cmd = format!("add server {}/{} {}", backend, server_name, addr);
    let resp = send_command(&cmd).await?;
    tracing::debug!(backend, server_name, addr, response = %resp, "add_server");
    Ok(())
}

/// Remove a dynamic server from a backend (server must be in maintenance mode first).
pub async fn remove_server(backend: &str, server_name: &str) -> Result<()> {
    let cmd = format!("del server {}/{}", backend, server_name);
    let resp = send_command(&cmd).await?;
    tracing::debug!(backend, server_name, response = %resp, "remove_server");
    Ok(())
}

/// Set the weight of a server within a backend.
pub async fn set_weight(backend: &str, server_name: &str, weight: u32) -> Result<()> {
    let cmd = format!("set weight {}/{} {}", backend, server_name, weight);
    let resp = send_command(&cmd).await?;
    tracing::debug!(backend, server_name, weight, response = %resp, "set_weight");
    Ok(())
}

/// Set the operational state of a server ("ready" or "maint").
pub async fn set_state(backend: &str, server_name: &str, state: &str) -> Result<()> {
    let cmd = format!("set server {}/{} state {}", backend, server_name, state);
    let resp = send_command(&cmd).await?;
    tracing::debug!(backend, server_name, state, response = %resp, "set_state");
    Ok(())
}

/// Update the address of an existing dynamic server.
///
/// `addr` must be in "ip:port" format. HAProxy keeps all other server
/// attributes (weight, state, …) unchanged.
pub async fn set_addr(backend: &str, server_name: &str, addr: &str) -> Result<()> {
    let (ip, port) = addr.rsplit_once(':').ok_or_else(|| {
        RuntimeError::Response(format!("invalid addr (expected ip:port): {}", addr))
    })?;
    let cmd = format!("set server {}/{} addr {} port {}", backend, server_name, ip, port);
    let resp = send_command(&cmd).await?;
    tracing::debug!(backend, server_name, addr, response = %resp, "set_addr");
    Ok(())
}
