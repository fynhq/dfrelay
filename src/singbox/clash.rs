use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub id: String,
    pub host: String,
    pub destination_ip: String,
    pub destination_port: String,
    pub network: String,
    pub upload: u64,
    pub download: u64,
    /// Seconds since the connection started; `None` when the API omitted it.
    #[serde(default)]
    pub duration_secs: Option<u64>,
}

impl ConnectionInfo {
    /// Destination shown in the connection list, without fallback text.
    pub fn target(&self) -> Option<String> {
        let host = self.host.trim();
        let ip = self.destination_ip.trim();
        let address = if !host.is_empty() {
            host
        } else if !ip.is_empty() {
            ip
        } else {
            return None;
        };
        if self.destination_port.is_empty() {
            Some(address.to_string())
        } else {
            Some(format!("{address}:{}", self.destination_port))
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxySelection {
    pub selected: Option<String>,
    pub auto_now: Option<String>,
    pub auto_delay: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub upload_total: u64,
    pub download_total: u64,
    pub connections: Vec<ConnectionInfo>,
    pub selection: ProxySelection,
    pub node_delays: HashMap<String, u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionsSnapshot {
    pub upload_total: u64,
    pub download_total: u64,
    pub connections: Vec<ConnectionInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct ProxiesSnapshot {
    pub selection: ProxySelection,
    pub node_delays: HashMap<String, u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum ClashError {
    #[error("HTTP {0}")]
    Http(u16),
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("invalid response: {0}")]
    Invalid(String),
}

#[derive(Clone)]
pub struct ClashClient {
    base: String,
    secret: String,
    client: Option<Client>,
    delay_client: Option<Client>,
}

impl ClashClient {
    pub fn new(port: u16, secret: impl Into<String>) -> Self {
        // Fail closed: the Clash API is loopback-only and carries a bearer
        // secret, so never fall back to a default client that may honor an
        // environment proxy. A client-build failure leaves requests disabled
        // and is reported through ClashError.
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .ok();
        // sing-box's delay endpoint uses a 5s timeout; allow enough extra time
        // for the 503/5xx response to arrive instead of treating every failed
        // probe as an unreachable Clash API.
        let delay_client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .ok();
        Self {
            base: format!("http://127.0.0.1:{port}"),
            secret: secret.into(),
            client,
            delay_client,
        }
    }

    fn http(&self) -> Result<&Client, ClashError> {
        self.client
            .as_ref()
            .ok_or_else(|| ClashError::Invalid("HTTP client unavailable".to_string()))
    }

    fn delay_http(&self) -> Result<&Client, ClashError> {
        self.delay_client
            .as_ref()
            .ok_or_else(|| ClashError::Invalid("HTTP client unavailable".to_string()))
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub async fn connections(&self) -> Result<ConnectionsSnapshot, ClashError> {
        let response: ConnectionsResponse = self.get_json("/connections").await?;
        Ok(response.into_snapshot())
    }

    pub async fn proxies(&self) -> Result<ProxiesSnapshot, ClashError> {
        let response: ProxiesResponse = self.get_json("/proxies").await?;
        Ok(response.into_snapshot())
    }

    pub async fn select(&self, name: &str) -> Result<(), ClashError> {
        let response = self
            .http()?
            .put(format!("{}/proxies/proxy", self.base))
            .bearer_auth(&self.secret)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await?;
        ensure_success(&response)?;
        Ok(())
    }

    pub async fn delay(&self, name: &str, test_url: &str) -> Result<u64, ClashError> {
        let url = delay_url(&self.base, name, test_url)?;
        let response = self
            .delay_http()?
            .get(url)
            .bearer_auth(&self.secret)
            .send()
            .await?;
        ensure_success(&response)?;
        let value: DelayResponse = response.json().await?;
        value
            .delay
            .ok_or_else(|| ClashError::Invalid("delay missing from response".to_string()))
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClashError> {
        let response = self
            .http()?
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.secret)
            .send()
            .await?;
        ensure_success(&response)?;
        response.json().await.map_err(ClashError::from)
    }
}

fn delay_url(base: &str, name: &str, test_url: &str) -> Result<url::Url, ClashError> {
    let mut url = url::Url::parse(base).map_err(|err| ClashError::Invalid(err.to_string()))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| ClashError::Invalid("cannot build delay URL".to_string()))?;
        segments.push("proxies").push(name).push("delay");
    }
    url.query_pairs_mut()
        .append_pair("timeout", "5000")
        .append_pair("url", test_url);
    Ok(url)
}

fn ensure_success(response: &reqwest::Response) -> Result<(), ClashError> {
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(ClashError::Http(status.as_u16()))
    }
}

#[derive(Debug, Default, Deserialize)]
struct ConnectionsResponse {
    #[serde(rename = "uploadTotal", default)]
    upload_total: u64,
    #[serde(rename = "downloadTotal", default)]
    download_total: u64,
    #[serde(default)]
    connections: Vec<RawConnection>,
}

impl ConnectionsResponse {
    fn into_snapshot(self) -> ConnectionsSnapshot {
        ConnectionsSnapshot {
            upload_total: self.upload_total,
            download_total: self.download_total,
            connections: self
                .connections
                .into_iter()
                .map(RawConnection::into_info)
                .collect(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawConnection {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    metadata: Option<RawMetadata>,
    #[serde(default)]
    upload: Option<u64>,
    #[serde(default)]
    download: Option<u64>,
    #[serde(default)]
    start: Option<String>,
}

impl RawConnection {
    fn into_info(self) -> ConnectionInfo {
        let metadata = self.metadata.unwrap_or_default();
        ConnectionInfo {
            id: self.id.unwrap_or_default(),
            host: crate::text::sanitize_display(metadata.host.as_deref().unwrap_or("")),
            destination_ip: crate::text::sanitize_display(
                metadata.destination_ip.as_deref().unwrap_or(""),
            ),
            destination_port: crate::text::sanitize_display(&metadata.destination_port),
            network: crate::text::sanitize_display(metadata.network.as_deref().unwrap_or("")),
            upload: self.upload.unwrap_or(0),
            download: self.download.unwrap_or(0),
            duration_secs: connection_duration(self.start.as_deref()),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawMetadata {
    #[serde(default)]
    host: Option<String>,
    #[serde(rename = "destinationIP", default)]
    destination_ip: Option<String>,
    #[serde(
        rename = "destinationPort",
        default,
        deserialize_with = "deserialize_string_or_number"
    )]
    destination_port: String,
    #[serde(default)]
    network: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ProxiesResponse {
    #[serde(default)]
    proxies: HashMap<String, ProxyEntry>,
}

impl ProxiesResponse {
    fn into_snapshot(self) -> ProxiesSnapshot {
        let selected = self
            .proxies
            .get("proxy")
            .and_then(|proxy| proxy.now.clone());
        let auto = self.proxies.get("auto");
        let auto_now = auto.and_then(|proxy| proxy.now.clone());
        let auto_delay = auto.and_then(ProxyEntry::last_delay);
        let mut node_delays = HashMap::new();
        for (tag, proxy) in &self.proxies {
            if let Some(delay) = proxy.last_delay().filter(|delay| *delay > 0) {
                node_delays.insert(tag.clone(), delay);
            }
        }
        ProxiesSnapshot {
            selection: ProxySelection {
                selected,
                auto_now,
                auto_delay,
            },
            node_delays,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ProxyEntry {
    #[serde(default)]
    now: Option<String>,
    #[serde(default)]
    history: Option<Vec<ProxyHistory>>,
}

impl ProxyEntry {
    fn last_delay(&self) -> Option<u64> {
        self.history.as_ref()?.last()?.delay
    }
}

#[derive(Debug, Default, Deserialize)]
struct ProxyHistory {
    #[serde(default)]
    delay: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct DelayResponse {
    #[serde(default)]
    delay: Option<u64>,
}

/// Clash metadata normally uses strings, but tolerate numeric ports too.
fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrNumber;

    impl serde::de::Visitor<'_> for StringOrNumber {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string, number, or null")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
            Ok(value.to_string())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
            Ok(value.to_string())
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(value.to_string())
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(String::new())
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(String::new())
        }
    }

    deserializer.deserialize_any(StringOrNumber)
}

/// Age of a connection, computed from the RFC3339 `start` field at snapshot
/// time. Clock skew that would yield a negative age clamps to zero.
fn connection_duration(start: Option<&str>) -> Option<u64> {
    let start = start.and_then(|start| DateTime::parse_from_rfc3339(start).ok())?;
    let seconds = Utc::now().signed_duration_since(start).num_seconds();
    Some(seconds.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn connection_target_prefers_host_then_ip() {
        let mut item = ConnectionInfo::default();
        assert_eq!(item.target(), None);
        item.host = " example.com ".to_string();
        item.destination_port = "443".to_string();
        assert_eq!(item.target().as_deref(), Some("example.com:443"));
        item.host.clear();
        item.destination_ip = "203.0.113.5".to_string();
        assert_eq!(item.target().as_deref(), Some("203.0.113.5:443"));
        item.destination_ip.clear();
        assert_eq!(item.target(), None);
    }

    #[test]
    fn parses_connections_and_selection() {
        let started = Utc::now() - chrono::TimeDelta::seconds(3661);
        let connections = json!({
            "uploadTotal": 100,
            "downloadTotal": 200,
            "memory": 1024,
            "connections": [{
                "id": "id1",
                "metadata": {
                    "network": "tcp",
                    "host": "example.com",
                    "destinationIP": "2001:db8::1",
                    "destinationPort": "443",
                    "sourceIP": "127.0.0.1",
                    "sourcePort": "50000",
                    "processPath": "/usr/bin/curl"
                },
                "upload": 10,
                "download": 20,
                "start": started.to_rfc3339(),
                "chains": ["proxy", "auto", "n-1a2b3c4d"],
                "rule": "final"
            }, {
                "id": "id2",
                "metadata": {
                    "network": "udp",
                    "host": "",
                    "destinationIP": "203.0.113.9",
                    "destinationPort": "443"
                },
                "upload": 0,
                "download": 0
            }]
        });
        let proxies = json!({
            "proxies": {
                "proxy": { "now": "auto", "history": [{ "delay": 42 }] },
                "auto": { "now": "n-1a2b3c4d", "history": [{ "delay": 42 }] },
                "n-1a2b3c4d": { "history": [{ "delay": 100 }, { "delay": 88 }] },
                "n-dead": { "history": [{ "delay": 0 }] }
            }
        });
        let connections: ConnectionsResponse = serde_json::from_value(connections).unwrap();
        let proxies: ProxiesResponse = serde_json::from_value(proxies).unwrap();
        let connections = connections.into_snapshot();
        let proxies = proxies.into_snapshot();
        assert_eq!(connections.upload_total, 100);
        assert_eq!(connections.download_total, 200);
        assert_eq!(connections.connections.len(), 2);
        assert_eq!(connections.connections[0].destination_ip, "2001:db8::1");
        let duration = connections.connections[0]
            .duration_secs
            .expect("duration parsed from start");
        assert!((3661..3671).contains(&duration), "{duration}");
        assert_eq!(connections.connections[1].duration_secs, None);
        assert_eq!(proxies.selection.selected.as_deref(), Some("auto"));
        assert_eq!(proxies.selection.auto_now.as_deref(), Some("n-1a2b3c4d"));
        assert_eq!(proxies.selection.auto_delay, Some(42));
        assert_eq!(proxies.node_delays.get("n-1a2b3c4d"), Some(&88));
        assert!(!proxies.node_delays.contains_key("n-dead"));
        assert_eq!(proxies.node_delays.get("auto"), Some(&42));
    }

    #[test]
    fn tolerates_missing_metadata_and_numeric_ports() {
        let connections: ConnectionsResponse = serde_json::from_value(json!({
            "uploadTotal": 1,
            "downloadTotal": 2,
            "connections": [{
                "id": "a",
                "metadata": {
                    "host": null,
                    "destinationIP": "203.0.113.4",
                    "destinationPort": 443,
                    "network": "tcp"
                },
                "upload": 3,
                "download": 4
            }, {
                "id": "b"
            }]
        }))
        .unwrap();
        let connections = connections.into_snapshot();
        assert_eq!(connections.connections.len(), 2);
        assert_eq!(connections.connections[0].host, "");
        assert_eq!(connections.connections[0].destination_port, "443");
        assert_eq!(
            connections.connections[0].target().as_deref(),
            Some("203.0.113.4:443")
        );
        assert_eq!(connections.connections[1].target(), None);
    }

    #[test]
    fn proxies_tolerate_missing_history() {
        let proxies: ProxiesResponse = serde_json::from_value(json!({
            "proxies": {
                "proxy": {},
                "auto": { "history": [] },
                "n-1": { "history": [{ "time": "2026-01-01T00:00:00Z" }] }
            }
        }))
        .unwrap();
        let proxies = proxies.into_snapshot();
        assert_eq!(proxies.selection.selected, None);
        assert_eq!(proxies.selection.auto_now, None);
        assert_eq!(proxies.selection.auto_delay, None);
        assert!(proxies.node_delays.is_empty());
    }

    #[test]
    fn base_url_is_loopback() {
        let client = ClashClient::new(12345, "secret");
        assert_eq!(client.base_url(), "http://127.0.0.1:12345");
    }

    #[test]
    fn delay_url_escapes_name_and_test_url() {
        let url = delay_url(
            "http://127.0.0.1:12345",
            "n-1a2b3c4d",
            "https://example.com/generate_204?x=1",
        )
        .unwrap();
        assert_eq!(url.path(), "/proxies/n-1a2b3c4d/delay");
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert!(pairs.contains(&("timeout".to_string(), "5000".to_string())));
        assert!(pairs.contains(&(
            "url".to_string(),
            "https://example.com/generate_204?x=1".to_string()
        )));
    }
}
