use std::env;
use std::sync::OnceLock;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::Mutex;

extern crate libc;

/// Serializes config edits + reloads. HAProxy's master can coalesce/drop a
/// SIGUSR2 that arrives while a previous reload is still forking its new worker,
/// which silently loses a backend (the file has it, the running process doesn't).
/// Holding this lock across the reload + a short settle prevents overlapping
/// reloads, so every config change reliably lands in the running process.
fn config_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// How long to wait after a reload before releasing the lock, giving the new
/// HAProxy worker time to come up before the next reload can fire.
const RELOAD_SETTLE_MS: u64 = 750;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frontend section not found in haproxy config — cannot insert ACL")]
    FrontendNotFound,
    #[error("haproxy config validation failed: {0}")]
    ValidationFailed(String),
    #[error("haproxy reload failed: {0}")]
    ReloadFailed(String),
}

pub type Result<T> = std::result::Result<T, ConfigError>;

fn cfg_path() -> String {
    env::var("HAPROXY_CFG").unwrap_or_else(|_| "/etc/haproxy/haproxy.cfg".to_string())
}

async fn read_config() -> Result<String> {
    Ok(tokio::fs::read_to_string(cfg_path()).await?)
}

async fn write_config(content: &str) -> Result<()> {
    tokio::fs::write(cfg_path(), content).await?;
    Ok(())
}

/// Validate the config with `haproxy -c -f <path>`. Returns the stderr output on failure.
async fn validate_config() -> Result<()> {
    let cfg = cfg_path();
    let output = Command::new("haproxy")
        .arg("-c")
        .arg("-f")
        .arg(&cfg)
        .output()
        .await?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(ConfigError::ValidationFailed(stderr))
    }
}

/// Reload HAProxy gracefully via SIGUSR2 to the master process.
async fn reload_haproxy() -> Result<()> {
    let pid_path = std::env::var("HAPROXY_PID")
        .unwrap_or_else(|_| "/run/haproxy/haproxy.pid".to_string());

    let pid_str = tokio::fs::read_to_string(&pid_path).await.map_err(|e| {
        ConfigError::ReloadFailed(format!("cannot read PID file {pid_path}: {e}"))
    })?;

    let pid: u32 = pid_str.trim().parse().map_err(|_| {
        ConfigError::ReloadFailed(format!("invalid PID in {pid_path}: {}", pid_str.trim()))
    })?;

    // Send SIGUSR2 directly via syscall — avoids dependency on an external `kill` binary
    // which is not present in debian:bookworm-slim (procps not installed).
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR2) };
    if rc == 0 {
        tracing::info!(pid, "HAProxy reloaded via SIGUSR2");
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        Err(ConfigError::ReloadFailed(format!("kill -USR2 {pid}: {err}")))
    }
}

/// Build the backend block text for a new service.
fn backend_block(service_name: &str) -> String {
    format!(
        "\nbackend {}\n    balance leastconn\n    option http-server-close\n    option httpchk GET /\n    http-check expect rstatus 2[0-9][0-9]\n    http-response set-header X-Served-By %[srv_name]\n",
        service_name
    )
}

/// Build the two frontend ACL lines for a service.
fn acl_lines(service_name: &str, domain: &str) -> [String; 2] {
    [
        format!("    acl host_{service_name} hdr(host) -i {domain}"),
        format!("    use_backend {service_name} if host_{service_name}"),
    ]
}

fn acl_marker(service_name: &str) -> String {
    format!("acl host_{service_name} ")
}

fn backend_header(service_name: &str) -> String {
    format!("backend {}", service_name)
}

/// Returns true if the service already has an ACL entry in the config.
pub fn service_exists(content: &str, service_name: &str) -> bool {
    let marker = acl_marker(service_name);
    content.lines().any(|l| l.trim().starts_with(&marker))
}

/// Insert service ACLs into the frontend section and append the backend block,
/// then validate and reload HAProxy.
///
/// Idempotent: returns Ok(()) without touching the config if the service already exists.
pub async fn add_service(service_name: &str, domain: &str) -> Result<()> {
    let _guard = config_lock().lock().await;
    let content = read_config().await?;

    if service_exists(&content, service_name) {
        tracing::debug!(service_name, "Service already in HAProxy config, skipping");
        return Ok(());
    }

    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

    // Insert ACL lines just before the first `default_backend` or existing `use_backend` line
    // so the routing rules are evaluated before the catch-all.
    let insert_pos = lines
        .iter()
        .position(|l| {
            let t = l.trim();
            t.starts_with("default_backend") || t.starts_with("use_backend")
        })
        .ok_or(ConfigError::FrontendNotFound)?;

    let [acl, use_be] = acl_lines(service_name, domain);
    lines.insert(insert_pos, use_be);
    lines.insert(insert_pos, acl);

    lines.push(backend_block(service_name));

    let new_content = lines.join("\n");

    // Write, validate; restore backup on failure.
    write_config(&new_content).await?;
    if let Err(e) = validate_config().await {
        tracing::error!(service_name, error = %e, "Config validation failed, restoring backup");
        write_config(&content).await?;
        return Err(e);
    }

    tracing::info!(service_name, domain, "Service added to HAProxy config");
    reload_haproxy().await?;
    tokio::time::sleep(std::time::Duration::from_millis(RELOAD_SETTLE_MS)).await;
    Ok(())
}

/// Remove service ACLs from the frontend section and the entire backend block,
/// then validate and reload HAProxy.
///
/// Idempotent: returns Ok(()) if the service does not exist.
pub async fn remove_service(service_name: &str) -> Result<()> {
    let _guard = config_lock().lock().await;
    let content = read_config().await?;

    if !service_exists(&content, service_name) {
        tracing::debug!(service_name, "Service not in HAProxy config, nothing to remove");
        return Ok(());
    }

    let lines: Vec<String> = content.lines().map(str::to_string).collect();

    let bh = backend_header(service_name);
    let acl_prefix = format!("    acl host_{} ", service_name);
    let use_prefix = format!("    use_backend {} ", service_name);

    let mut in_target_backend = false;
    let mut filtered: Vec<String> = Vec::with_capacity(lines.len());

    for line in &lines {
        // A non-indented, non-empty line starts a new config section.
        if !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty() {
            in_target_backend = line.trim() == bh;
        }

        if in_target_backend {
            continue;
        }
        if line.starts_with(&acl_prefix) || line.starts_with(&use_prefix) {
            continue;
        }

        filtered.push(line.clone());
    }

    let new_content = filtered.join("\n");

    write_config(&new_content).await?;
    if let Err(e) = validate_config().await {
        tracing::error!(service_name, error = %e, "Config validation failed, restoring backup");
        write_config(&content).await?;
        return Err(e);
    }

    tracing::info!(service_name, "Service removed from HAProxy config");
    reload_haproxy().await?;
    tokio::time::sleep(std::time::Duration::from_millis(RELOAD_SETTLE_MS)).await;
    Ok(())
}

/// Return the list of service backend names currently in the config.
/// Collects all `backend svc_*` entries.
pub async fn list_services() -> Result<Vec<String>> {
    let content = read_config().await?;
    let services = content
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            if t.starts_with("backend svc_") {
                Some(t["backend ".len()..].to_string())
            } else {
                None
            }
        })
        .collect();
    Ok(services)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CFG: &str = "\
global
    daemon

defaults
    mode http

frontend openshrd
    bind *:80
    default_backend volunteers

backend volunteers
    balance leastconn
";

    const CFG_WITH_SVC: &str = "\
global
    daemon

defaults
    mode http

frontend openshrd
    bind *:80
    acl host_svc_foo hdr(host) -i foo.example.com
    use_backend svc_foo if host_svc_foo
    default_backend volunteers

backend volunteers
    balance leastconn

backend svc_foo
    balance leastconn
    option httpchk GET /health
    http-check expect status 200
";

    #[test]
    fn service_exists_detects_present() {
        assert!(service_exists(CFG_WITH_SVC, "svc_foo"));
    }

    #[test]
    fn service_exists_detects_absent() {
        assert!(!service_exists(SAMPLE_CFG, "svc_foo"));
        assert!(!service_exists(CFG_WITH_SVC, "svc_bar"));
    }
}
