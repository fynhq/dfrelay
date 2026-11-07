use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use reqwest::Client;
use url::Host;

pub const DEFAULT_USER_AGENT: &str = "clash.meta";
pub const DEFAULT_MAX_BYTES: usize = 10 * 1024 * 1024;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum number of redirect hops followed for a subscription fetch.
const MAX_REDIRECTS: usize = 5;

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
    #[error("redirect to a non-public address is not allowed")]
    BlockedRedirect,
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
}

pub async fn fetch_subscription(url: &str, options: &FetchOptions) -> Result<String, FetchError> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(FetchError::InvalidUrl);
    }
    let mut current = url::Url::parse(url).map_err(|_| FetchError::InvalidUrl)?;

    // Redirects are followed manually so every hop can be checked against the
    // private/loopback/link-local ranges. The initial URL is user-provided and
    // may legitimately point at a LAN address; only provider-controlled
    // redirect targets are restricted.
    for hop in 0..=MAX_REDIRECTS {
        if current.scheme() != "http" && current.scheme() != "https" {
            return Err(FetchError::InvalidUrl);
        }
        // Resolve once and pin the same addresses for validation and for the
        // actual connection. Resolving again in `fetch_hop` would leave a
        // DNS-rebinding window between the redirect check and the request.
        let host = current.host().ok_or(FetchError::InvalidUrl)?;
        let port = current
            .port_or_known_default()
            .ok_or(FetchError::InvalidUrl)?;
        let addrs = match &host {
            Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(*ip), port)],
            Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(*ip), port)],
            Host::Domain(domain) => resolve_host(domain, port).await?,
        };
        // The initial URL is user-provided and may legitimately point at a
        // LAN address; only provider-controlled redirect targets are
        // restricted.
        if hop > 0 && addrs.iter().any(|addr| is_blocked_target(&addr.ip())) {
            return Err(FetchError::BlockedRedirect);
        }
        let response = fetch_hop(&current, options, host, &addrs).await?;
        if response.status().is_redirection() {
            if hop == MAX_REDIRECTS {
                return Err(FetchError::Http(response.status().as_u16()));
            }
            let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                return Err(FetchError::Http(response.status().as_u16()));
            };
            let target = current
                .join(location.to_str().map_err(|_| FetchError::InvalidUrl)?)
                .map_err(|_| FetchError::InvalidUrl)?;
            current = target;
            continue;
        }
        let status = response.status();
        if !status.is_success() {
            return Err(FetchError::Http(status.as_u16()));
        }
        return read_body(response, options).await;
    }
    Err(FetchError::InvalidUrl)
}

async fn fetch_hop(
    url: &url::Url,
    options: &FetchOptions,
    host: Host<&str>,
    addrs: &[SocketAddr],
) -> Result<reqwest::Response, FetchError> {
    let mut builder = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(options.timeout)
        .user_agent(&options.user_agent);
    if let Host::Domain(domain) = host {
        builder = builder.resolve_to_addrs(domain, addrs);
    }
    let client = builder.build()?;
    client
        .get(url.clone())
        .send()
        .await
        .map_err(redact_request_error)
}

async fn read_body(
    mut response: reqwest::Response,
    options: &FetchOptions,
) -> Result<String, FetchError> {
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

/// Reject non-public IPv4/IPv6 destinations.
fn is_blocked_target(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || octets[0] == 0 // 0.0.0.0/8 ("this network")
                || (octets[0] & 0xf0) == 0xf0 // 240.0.0.0/4
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0) // 192.0.0.0/24
                || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)) // 198.18.0.0/15
                || (octets[0] == 100 && (64..=127).contains(&octets[1])) // CGNAT (RFC 6598)
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00 // unique local (fc00::/7)
                || (segments[0] & 0xffc0) == 0xfe80 // link-local (fe80::/10)
                || (segments[0] & 0xffc0) == 0xfec0 // site-local (fec0::/10)
                || (segments[0] == 0x0064
                    && segments[1] == 0xff9b
                    && segments[2..6].iter().all(|segment| *segment == 0)) // NAT64 (64:ff9b::/96)
                || v6.to_ipv4()
                    .is_some_and(|v4| is_blocked_target(&IpAddr::V4(v4)))
        }
    }
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

    #[test]
    fn blocks_private_and_special_redirect_targets() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        // Constructed numerically: these are protocol ranges (loopback,
        // RFC 1918, link-local, CGNAT, ULA), not real hosts.
        let blocked = [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V4(Ipv4Addr::new(0, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(198, 18, 0, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0x7f00, 1)),
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 1)),
        ];
        for ip in blocked {
            assert!(is_blocked_target(&ip), "{ip} should be blocked");
        }
    }

    #[test]
    fn allows_public_redirect_targets() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        let allowed = [
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)),
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)),
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
        ];
        for ip in allowed {
            assert!(!is_blocked_target(&ip), "{ip} should be allowed");
        }
    }
}
