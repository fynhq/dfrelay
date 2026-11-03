use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    client: Client,
    delay_client: Client,
}

impl ClashClient {
    pub fn new(port: u16, secret: impl Into<String>) -> Self {
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| Client::new());
        // sing-box's delay endpoint uses a 5s timeout; allow enough extra time
        // for the 503/5xx response to arrive instead of treating every failed
        // probe as an unreachable Clash API.
        let delay_client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            base: format!("http://127.0.0.1:{port}"),
            secret: secret.into(),
            client,
            delay_client,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub async fn metrics(&self) -> Result<MetricsSnapshot, ClashError> {
        let connections = self.get_json("/connections").await?;
        let proxies = self.get_json("/proxies").await?;
        Ok(parse_metrics(
            &connections,
            parse_selection(&proxies),
            parse_node_delays(&proxies),
        ))
    }

    pub async fn select(&self, name: &str) -> Result<(), ClashError> {
        let response = self
            .client
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
            .delay_client
            .get(url)
            .bearer_auth(&self.secret)
            .send()
            .await?;
        ensure_success(&response)?;
        let value: Value = response.json().await?;
        value
            .get("delay")
            .and_then(Value::as_u64)
            .ok_or_else(|| ClashError::Invalid("delay missing from response".to_string()))
    }

    async fn get_json(&self, path: &str) -> Result<Value, ClashError> {
        let response = self
            .client
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

fn parse_metrics(
    value: &Value,
    selection: ProxySelection,
    node_delays: HashMap<String, u64>,
) -> MetricsSnapshot {
    let connections = value
        .get("connections")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_connection).collect())
        .unwrap_or_default();
    MetricsSnapshot {
        upload_total: value
            .get("uploadTotal")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        download_total: value
            .get("downloadTotal")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        connections,
        selection,
        node_delays,
    }
}

fn parse_node_delays(value: &Value) -> HashMap<String, u64> {
    let mut delays = HashMap::new();
    let Some(proxies) = value.get("proxies").and_then(Value::as_object) else {
        return delays;
    };
    for (tag, proxy) in proxies {
        let Some(delay) = proxy
            .get("history")
            .and_then(Value::as_array)
            .and_then(|history| history.last())
            .and_then(|item| item.get("delay"))
            .and_then(Value::as_u64)
        else {
            continue;
        };
        if delay > 0 {
            delays.insert(tag.clone(), delay);
        }
    }
    delays
}

fn parse_connection(value: &Value) -> Option<ConnectionInfo> {
    let metadata = value.get("metadata")?;
    Some(ConnectionInfo {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        host: crate::text::sanitize_display(
            metadata.get("host").and_then(Value::as_str).unwrap_or(""),
        ),
        destination_ip: crate::text::sanitize_display(
            metadata
                .get("destinationIP")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        destination_port: crate::text::sanitize_display(
            metadata
                .get("destinationPort")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        network: crate::text::sanitize_display(
            metadata
                .get("network")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        upload: value.get("upload").and_then(Value::as_u64).unwrap_or(0),
        download: value.get("download").and_then(Value::as_u64).unwrap_or(0),
        duration_secs: connection_duration(value),
    })
}

/// Age of a connection, computed from the RFC3339 `start` field at snapshot
/// time. Clock skew that would yield a negative age clamps to zero.
fn connection_duration(value: &Value) -> Option<u64> {
    let start = value
        .get("start")
        .and_then(Value::as_str)
        .and_then(|start| DateTime::parse_from_rfc3339(start).ok())?;
    let seconds = Utc::now().signed_duration_since(start).num_seconds();
    Some(seconds.max(0) as u64)
}

fn parse_selection(value: &Value) -> ProxySelection {
    let proxies = value.get("proxies");
    let selected = proxies
        .and_then(|p| p.get("proxy"))
        .and_then(|p| p.get("now"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let auto = proxies.and_then(|p| p.get("auto"));
    let auto_now = auto
        .and_then(|a| a.get("now"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let auto_delay = auto
        .and_then(|a| a.get("history"))
        .and_then(Value::as_array)
        .and_then(|history| history.last())
        .and_then(|item| item.get("delay"))
        .and_then(Value::as_u64);
    ProxySelection {
        selected,
        auto_now,
        auto_delay,
    }
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
        let snapshot = parse_metrics(
            &connections,
            parse_selection(&proxies),
            parse_node_delays(&proxies),
        );
        assert_eq!(snapshot.upload_total, 100);
        assert_eq!(snapshot.download_total, 200);
        assert_eq!(snapshot.connections.len(), 2);
        assert_eq!(snapshot.connections[0].destination_ip, "2001:db8::1");
        let duration = snapshot.connections[0]
            .duration_secs
            .expect("duration parsed from start");
        assert!((3661..3671).contains(&duration), "{duration}");
        assert_eq!(snapshot.connections[1].duration_secs, None);
        assert_eq!(snapshot.selection.selected.as_deref(), Some("auto"));
        assert_eq!(snapshot.selection.auto_now.as_deref(), Some("n-1a2b3c4d"));
        assert_eq!(snapshot.selection.auto_delay, Some(42));
        assert_eq!(snapshot.node_delays.get("n-1a2b3c4d"), Some(&88));
        assert!(!snapshot.node_delays.contains_key("n-dead"));
        assert_eq!(snapshot.node_delays.get("auto"), Some(&42));
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
