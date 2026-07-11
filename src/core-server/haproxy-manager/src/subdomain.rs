use std::env;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DnsError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("cloudflare API error: {0}")]
    Api(String),
}

pub type Result<T> = std::result::Result<T, DnsError>;

/// Abstraction over a DNS provider for subdomain lifecycle management.
#[allow(async_fn_in_trait)]
pub trait DnsProvider: Send + Sync {
    async fn create(&self, subdomain: &str) -> Result<()>;
    async fn delete(&self, subdomain: &str) -> Result<()>;
}

/// No-op provider for local development and wildcard-covered deployments.
///
/// Since *.openshrd.danlara.com.br is already covered by a Cloudflare Tunnel
/// wildcard, subdomains do not need individual DNS records.
pub struct NoopProvider;

impl DnsProvider for NoopProvider {
    async fn create(&self, subdomain: &str) -> Result<()> {
        tracing::debug!(subdomain, "NoopProvider: skipping DNS create");
        Ok(())
    }

    async fn delete(&self, subdomain: &str) -> Result<()> {
        tracing::debug!(subdomain, "NoopProvider: skipping DNS delete");
        Ok(())
    }
}

/// Cloudflare DNS provider using the Cloudflare API v4.
///
/// Required environment variables:
/// - `CLOUDFLARE_TOKEN`   — API token with DNS edit permission
/// - `CLOUDFLARE_ZONE_ID` — Zone ID for the target domain
/// - `BASE_DOMAIN`        — Base domain (e.g. openshrd.danlara.com.br)
pub struct CloudflareProvider {
    client: reqwest::Client,
    token: String,
    zone_id: String,
    base_domain: String,
}

impl CloudflareProvider {
    /// Build a provider from environment variables.
    pub fn from_env() -> Option<Self> {
        let token = env::var("CLOUDFLARE_TOKEN").ok()?;
        let zone_id = env::var("CLOUDFLARE_ZONE_ID").ok()?;
        let base_domain = env::var("BASE_DOMAIN").ok()?;
        Some(Self {
            client: reqwest::Client::new(),
            token,
            zone_id,
            base_domain,
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("https://api.cloudflare.com/client/v4{}", path)
    }
}

impl DnsProvider for CloudflareProvider {
    /// Create an A record pointing `subdomain.BASE_DOMAIN` to the core server.
    ///
    /// The IP is resolved from the `SERVER_IP` environment variable, or defaults
    /// to `127.0.0.1` if unset (useful for testing).
    async fn create(&self, subdomain: &str) -> Result<()> {
        let fqdn = format!("{}.{}", subdomain, self.base_domain);
        let ip = env::var("SERVER_IP").unwrap_or_else(|_| "127.0.0.1".to_string());

        let body = serde_json::json!({
            "type": "A",
            "name": fqdn,
            "content": ip,
            "ttl": 1,
            "proxied": false
        });

        let url = self.api_url(&format!("/zones/{}/dns_records", self.zone_id));
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        if resp["success"].as_bool() != Some(true) {
            let errors = resp["errors"].to_string();
            return Err(DnsError::Api(errors));
        }

        tracing::info!(subdomain, fqdn, "DNS A record created via Cloudflare");
        Ok(())
    }

    /// Delete the A record for `subdomain.BASE_DOMAIN`.
    async fn delete(&self, subdomain: &str) -> Result<()> {
        let fqdn = format!("{}.{}", subdomain, self.base_domain);

        // List records matching the name to find the record ID.
        let list_url = self.api_url(&format!(
            "/zones/{}/dns_records?type=A&name={}",
            self.zone_id, fqdn
        ));
        let resp = self
            .client
            .get(&list_url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        let record_id = resp["result"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|r| r["id"].as_str())
            .map(str::to_string);

        let record_id = match record_id {
            Some(id) => id,
            None => {
                tracing::warn!(subdomain, fqdn, "DNS record not found, nothing to delete");
                return Ok(());
            }
        };

        let delete_url =
            self.api_url(&format!("/zones/{}/dns_records/{}", self.zone_id, record_id));
        let resp = self
            .client
            .delete(&delete_url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        if resp["success"].as_bool() != Some(true) {
            let errors = resp["errors"].to_string();
            return Err(DnsError::Api(errors));
        }

        tracing::info!(subdomain, fqdn, "DNS A record deleted via Cloudflare");
        Ok(())
    }
}
