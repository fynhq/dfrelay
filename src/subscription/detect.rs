use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_yaml_ng::Value as YamlValue;

use super::model::Node;
use super::parse_clash::parse_clash_document;
use super::parse_uri::{decode_base64_text, parse_uri_list};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsupportedNode {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ParseOutcome {
    pub nodes: Vec<Node>,
    pub unsupported: Vec<UnsupportedNode>,
}

pub fn detect_and_parse(body: &str, source: &str) -> Result<ParseOutcome, String> {
    let trimmed = body.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err("empty subscription body".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("<!doctype") || lower.contains("<html") {
        return Err("subscription returned an HTML page; check URL or User-Agent".to_string());
    }

    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(outcome) = parse_sing_box_json(&value, source) {
                return Ok(outcome);
            }
        }
    }

    if let Ok(document) = serde_yaml_ng::from_str::<YamlValue>(trimmed) {
        let proxies_key = YamlValue::String("proxies".to_string());
        let has_proxies = document
            .as_mapping()
            .is_some_and(|mapping| mapping.contains_key(&proxies_key));
        if has_proxies {
            let (nodes, unsupported) = parse_clash_document(&document, source)?;
            return Ok(ParseOutcome {
                nodes,
                unsupported: unsupported
                    .into_iter()
                    .map(|(name, reason)| UnsupportedNode { name, reason })
                    .collect(),
            });
        }
    }

    if let Some(decoded) = decode_base64_text(trimmed) {
        if decoded.contains("://") {
            let (nodes, unsupported) = parse_uri_list(&decoded, source);
            if !nodes.is_empty() || !unsupported.is_empty() {
                return Ok(ParseOutcome {
                    nodes,
                    unsupported: unsupported
                        .into_iter()
                        .map(|(name, reason)| UnsupportedNode { name, reason })
                        .collect(),
                });
            }
        }
    }

    if trimmed.contains("://") {
        let (nodes, unsupported) = parse_uri_list(trimmed, source);
        if !nodes.is_empty() || !unsupported.is_empty() {
            return Ok(ParseOutcome {
                nodes,
                unsupported: unsupported
                    .into_iter()
                    .map(|(name, reason)| UnsupportedNode { name, reason })
                    .collect(),
            });
        }
    }

    Err("unrecognized subscription format".to_string())
}

fn validate_sing_box_node(
    object: &serde_json::Map<String, Value>,
    kind: &str,
) -> Result<(), String> {
    let require = |field: &str| {
        object
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|_| ())
            .ok_or_else(|| format!("{kind} missing {field}"))
    };
    match kind {
        "shadowsocks" => {
            require("method")?;
            require("password")?;
        }
        "vmess" | "vless" => require("uuid")?,
        "trojan" | "hysteria2" | "anytls" => require("password")?,
        "tuic" => {
            require("uuid")?;
            require("password")?;
        }
        "shadowtls" => require("password")?,
        "ssh" => require("user")?,
        "http" | "socks" | "hysteria" | "tor" => {}
        other => return Err(format!("unsupported protocol: {other}")),
    }
    Ok(())
}

fn parse_sing_box_json(value: &Value, source: &str) -> Option<ParseOutcome> {
    let outbounds = value
        .get("outbounds")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())?;
    let mut nodes = Vec::new();
    let mut unsupported = Vec::new();
    for outbound in outbounds {
        let Some(object) = outbound.as_object() else {
            continue;
        };
        let Some(kind) = object.get("type").and_then(Value::as_str) else {
            continue;
        };
        let Some(server) = object.get("server").and_then(Value::as_str) else {
            continue;
        };
        let Some(port) = object.get("server_port").and_then(Value::as_u64) else {
            continue;
        };
        let name = object
            .get("tag")
            .and_then(Value::as_str)
            .unwrap_or(kind)
            .to_string();
        let port = match u16::try_from(port) {
            Ok(port) if port > 0 => port,
            _ => {
                unsupported.push(UnsupportedNode {
                    name,
                    reason: "invalid server_port".to_string(),
                });
                continue;
            }
        };
        if let Err(reason) = validate_sing_box_node(object, kind) {
            unsupported.push(UnsupportedNode { name, reason });
            continue;
        }
        let mut outbound = outbound.clone();
        if let Some(object) = outbound.as_object_mut() {
            object.remove("tag");
        }
        nodes.push(Node::new(
            name,
            source.to_string(),
            kind.to_string(),
            server.to_string(),
            port,
            outbound,
        ));
    }
    if nodes.is_empty() && unsupported.is_empty() {
        return None;
    }
    Some(ParseOutcome { nodes, unsupported })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    #[test]
    fn detects_clash_yaml() {
        let outcome = detect_and_parse(
            "proxies:\n  - name: a\n    type: ss\n    server: example.com\n    port: 443\n    cipher: aes-128-gcm\n    password: p\n",
            "test",
        )
        .unwrap();
        assert_eq!(outcome.nodes.len(), 1);
    }

    #[test]
    fn detects_base64_uri_list() {
        let uri = "ss://YWVzLTEyOC1nY206c2VjcmV0@example.com:443#a";
        let body = STANDARD.encode(format!("{uri}\nwireguard://bad"));
        let outcome = detect_and_parse(&body, "test").unwrap();
        assert_eq!(outcome.nodes.len(), 1);
        assert_eq!(outcome.unsupported.len(), 1);
    }

    #[test]
    fn detects_sing_box_json() {
        let body = serde_json::json!({
            "outbounds": [{
                "type": "shadowsocks", "tag": "n1", "server": "example.com",
                "server_port": 443, "method": "aes-128-gcm", "password": "p"
            }]
        })
        .to_string();
        let outcome = detect_and_parse(&body, "test").unwrap();
        assert_eq!(outcome.nodes.len(), 1);
    }

    #[test]
    fn rejects_html() {
        let err = detect_and_parse("<html>login</html>", "test").unwrap_err();
        assert!(err.contains("HTML"));
    }

    #[test]
    fn rejects_unknown_content() {
        assert!(detect_and_parse("hello world", "test").is_err());
    }

    #[test]
    fn reports_unsupported_or_incomplete_sing_box_nodes() {
        let body = serde_json::json!({
            "outbounds": [
                {
                    "type": "wireguard", "tag": "wg", "server": "example.com",
                    "server_port": 51820
                },
                {
                    "type": "shadowsocks", "tag": "ss", "server": "example.com",
                    "server_port": 443, "password": "p"
                },
                {
                    "type": "shadowsocks", "tag": "good", "server": "example.com",
                    "server_port": 443, "method": "aes-128-gcm", "password": "p"
                }
            ]
        })
        .to_string();
        let outcome = detect_and_parse(&body, "test").unwrap();
        assert_eq!(outcome.nodes.len(), 1);
        assert_eq!(outcome.unsupported.len(), 2);
        assert!(outcome
            .unsupported
            .iter()
            .any(|item| item.reason.contains("wireguard")));
        assert!(outcome
            .unsupported
            .iter()
            .any(|item| item.reason.contains("missing method")));
    }
}
