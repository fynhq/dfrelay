use std::net::SocketAddr;
use std::time::Duration;

use reqwest::Client;
use url::Host;

pub const DEFAULT_USER_AGENT: &str = "clash.meta";
pub const DEFAULT_MAX_BYTES: usize = 10 * 1024 * 1024;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub user_agent: String,
    pub timeout: Duration,
    pub max_bytes: usize,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            timeout: Duration::from_secs(30),
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("invalid subscription URL")]
    InvalidUrl,
    #[error("HTTP {0}")]
    Http(u16),
    #[error("subscription body is too large")]
    TooLarge,
    #[error("subscription body is empty")]
    Empty,
    #[error("DNS resolution failed: {0}")]
    Resolve(String),
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
}

pub async fn fetch_subscription(url: &str, options: &FetchOptions) -> Result<String, FetchError> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(FetchError::InvalidUrl);
    }
    let parsed = url::Url::parse(url).map_err(|_| FetchError::InvalidUrl)?;
    let host = parsed.host().ok_or(FetchError::InvalidUrl)?;
    let port = parsed
        .port_or_known_default()
        .ok_or(FetchError::InvalidUrl)?;

    let mut builder = Client::builder()
        .no_proxy()
        .timeout(options.timeout)
        .user_agent(&options.user_agent);
    if let Host::Domain(domain) = host {
        let addrs = resolve_host(domain, port).await?;
        builder = builder.resolve_to_addrs(domain, &addrs);
    }
    let client = builder.build()?;
    let mut response = client
        .get(parsed)
        .send()
        .await
        .map_err(redact_request_error)?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(redact_request_error)? {
        if bytes.len().saturating_add(chunk.len()) > options.max_bytes {
            return Err(FetchError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&bytes).to_string();
    if text.trim().is_empty() {
        return Err(FetchError::Empty);
    }
    Ok(text)
}

fn redact_request_error(err: reqwest::Error) -> FetchError {
    FetchError::Request(err.without_url())
}

async fn resolve_host(host: &str, port: u16) -> Result<Vec<SocketAddr>, FetchError> {
    let lookup = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| FetchError::Resolve(format!("{host}: timeout")))?
        .map_err(|err| FetchError::Resolve(format!("{host}: {err}")))?;
    let addrs: Vec<SocketAddr> = lookup.collect();
    if addrs.is_empty() {
        return Err(FetchError::Resolve(format!("{host}: no address")));
    }
    Ok(addrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_http_url() {
        let err = fetch_subscription("file:///etc/passwd", &FetchOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::InvalidUrl));
    }

    #[tokio::test]
    async fn request_errors_do_not_leak_the_url() {
        let options = FetchOptions {
            timeout: Duration::from_millis(500),
            ..FetchOptions::default()
        };
        let err = fetch_subscription("http://127.0.0.1:1/sub?token=secret", &options)
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("secret"), "{text}");
        assert!(!text.contains("/sub"), "{text}");
    }
}
