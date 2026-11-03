use serde_json::{json, Map, Value};
use serde_yaml_ng::{Mapping, Value as YamlValue};

use super::model::Node;

pub type ClashParseOutput = (Vec<Node>, Vec<(String, String)>);

#[cfg(test)]
pub fn parse_clash(text: &str, source: &str) -> Result<ClashParseOutput, String> {
    let document: YamlValue =
        serde_yaml_ng::from_str(text).map_err(|e| format!("invalid YAML: {e}"))?;
    parse_clash_document(&document, source)
}

pub fn parse_clash_document(
    document: &YamlValue,
    source: &str,
) -> Result<ClashParseOutput, String> {
    let proxies = document
        .as_mapping()
        .and_then(|mapping| yaml_get(mapping, "proxies"))
        .and_then(YamlValue::as_sequence)
        .ok_or("Clash YAML has no proxies list")?;

    let mut nodes = Vec::new();
    let mut unsupported = Vec::new();
    for proxy in proxies {
        let Some(map) = proxy.as_mapping() else {
            continue;
        };
        let name = yaml_str(map, "name").unwrap_or("unnamed").to_string();
        match parse_proxy(map, source) {
            Ok(node) => nodes.push(node),
            Err(reason) => unsupported.push((name, reason)),
        }
    }
    Ok((nodes, unsupported))
}

fn parse_proxy(proxy: &Mapping, source: &str) -> Result<Node, String> {
    let kind = yaml_str(proxy, "type").ok_or("missing type")?;
    let server = yaml_str(proxy, "server")
        .ok_or("missing server")?
        .to_string();
    if server.is_empty() {
        return Err("missing server".to_string());
    }
    let port = yaml_u16(proxy, "port").ok_or("missing or invalid port")?;
    if port == 0 {
        return Err("invalid port".to_string());
    }
    let name = yaml_str(proxy, "name")
        .unwrap_or(&format!("{server}:{port}"))
        .to_string();
    let network = yaml_str(proxy, "network")
        .filter(|network| !network.is_empty())
        .unwrap_or("tcp");
    if matches!(kind, "vmess" | "vless" | "trojan") {
        ensure_supported_network(network)?;
    }

    let (protocol, mut outbound) = match kind {
        "ss" => {
            if yaml_str(proxy, "plugin").is_some() {
                return Err("shadowsocks plugin is not supported".to_string());
            }
            let cipher = yaml_str(proxy, "cipher").ok_or("ss missing cipher")?;
            let password = yaml_str(proxy, "password").ok_or("ss missing password")?;
            if cipher.is_empty() || password.is_empty() {
                return Err("ss cipher or password is empty".to_string());
            }
            (
                "shadowsocks",
                json!({
                    "type": "shadowsocks",
                    "server": server,
                    "server_port": port,
                    "method": cipher,
                    "password": password
                }),
            )
        }
        "vmess" => {
            let uuid = yaml_str(proxy, "uuid").ok_or("vmess missing uuid")?;
            if uuid.is_empty() {
                return Err("vmess missing uuid".to_string());
            }
            let alter_id = yaml_u16(proxy, "alterId").unwrap_or(0);
            let security = yaml_str(proxy, "cipher").unwrap_or("auto");
            let mut value = Map::new();
            value.insert("type".into(), json!("vmess"));
            value.insert("server".into(), json!(server));
            value.insert("server_port".into(), json!(port));
            value.insert("uuid".into(), json!(uuid));
            value.insert("security".into(), json!(security));
            if alter_id > 0 {
                value.insert("alter_id".into(), json!(alter_id));
            }
            attach_transport_tls(proxy, network, &mut value);
            ("vmess", Value::Object(value))
        }
        "vless" => {
            let uuid = yaml_str(proxy, "uuid").ok_or("vless missing uuid")?;
            if uuid.is_empty() {
                return Err("vless missing uuid".to_string());
            }
            let mut value = Map::new();
            value.insert("type".into(), json!("vless"));
            value.insert("server".into(), json!(server));
            value.insert("server_port".into(), json!(port));
            value.insert("uuid".into(), json!(uuid));
            if let Some(flow) = yaml_str(proxy, "flow") {
                value.insert("flow".into(), json!(flow));
            }
            attach_transport_tls(proxy, network, &mut value);
            ("vless", Value::Object(value))
        }
        "trojan" => {
            let password = yaml_str(proxy, "password").ok_or("trojan missing password")?;
            if password.is_empty() {
                return Err("trojan missing password".to_string());
            }
            let mut value = Map::new();
            value.insert("type".into(), json!("trojan"));
            value.insert("server".into(), json!(server));
            value.insert("server_port".into(), json!(port));
            value.insert("password".into(), json!(password));
            attach_transport(proxy, network, &mut value);
            value.insert("tls".into(), tls_value(proxy, true));
            ("trojan", Value::Object(value))
        }
        "hysteria2" | "hy2" => {
            let password = yaml_str(proxy, "password").ok_or("hysteria2 missing password")?;
            if password.is_empty() {
                return Err("hysteria2 missing password".to_string());
            }
            let mut value = Map::new();
            value.insert("type".into(), json!("hysteria2"));
            value.insert("server".into(), json!(server));
            value.insert("server_port".into(), json!(port));
            value.insert("password".into(), json!(password));
            if let Some(obfs) = yaml_str(proxy, "obfs") {
                let mut obfs_value = Map::new();
                obfs_value.insert("type".into(), json!(obfs));
                if let Some(password) = yaml_str(proxy, "obfs-password") {
                    obfs_value.insert("password".into(), json!(password));
                }
                value.insert("obfs".into(), Value::Object(obfs_value));
            }
            value.insert("tls".into(), tls_value(proxy, true));
            ("hysteria2", Value::Object(value))
        }
        "tuic" => {
            let uuid = yaml_str(proxy, "uuid").ok_or("tuic missing uuid")?;
            let password = yaml_str(proxy, "password").ok_or("tuic missing password")?;
            if uuid.is_empty() || password.is_empty() {
                return Err("tuic missing uuid or password".to_string());
            }
            let mut value = Map::new();
            value.insert("type".into(), json!("tuic"));
            value.insert("server".into(), json!(server));
            value.insert("server_port".into(), json!(port));
            value.insert("uuid".into(), json!(uuid));
            value.insert("password".into(), json!(password));
            if let Some(congestion) = yaml_str(proxy, "congestion-controller") {
                value.insert("congestion_control".into(), json!(congestion));
            }
            if let Some(mode) = yaml_str(proxy, "udp-relay-mode") {
                value.insert("udp_relay_mode".into(), json!(mode));
            }
            value.insert("tls".into(), tls_value(proxy, true));
            ("tuic", Value::Object(value))
        }
        "http" => {
            let mut value = Map::new();
            value.insert("type".into(), json!("http"));
            value.insert("server".into(), json!(server));
            value.insert("server_port".into(), json!(port));
            add_auth(proxy, &mut value);
            if tls_enabled(proxy) {
                value.insert("tls".into(), tls_value(proxy, true));
            }
            ("http", Value::Object(value))
        }
        "socks5" | "socks" => {
            let mut value = Map::new();
            value.insert("type".into(), json!("socks"));
            value.insert("server".into(), json!(server));
            value.insert("server_port".into(), json!(port));
            add_auth(proxy, &mut value);
            ("socks", Value::Object(value))
        }
        "anytls" => {
            if ["shadow-tls-opts", "restls-opts", "jls-opts"]
                .iter()
                .any(|key| yaml_get(proxy, key).is_some())
            {
                return Err("anytls camouflage variant is not supported".to_string());
            }
            let password = yaml_str(proxy, "password")
                .filter(|password| !password.is_empty())
                .ok_or("anytls missing password")?;
            let mut value = Map::new();
            value.insert("type".into(), json!("anytls"));
            value.insert("server".into(), json!(server));
            value.insert("server_port".into(), json!(port));
            value.insert("password".into(), json!(password));
            insert_duration(
                proxy,
                "idle-session-check-interval",
                "idle_session_check_interval",
                &mut value,
            );
            insert_duration(
                proxy,
                "idle-session-timeout",
                "idle_session_timeout",
                &mut value,
            );
            if let Some(min) = yaml_u64(proxy, "min-idle-session") {
                value.insert("min_idle_session".into(), json!(min));
            }
            // sing-box requires TLS for anytls; map sni/alpn/insecure/utls.
            value.insert("tls".into(), tls_value(proxy, true));
            ("anytls", Value::Object(value))
        }
        other => return Err(format!("unsupported protocol: {other}")),
    };

    if let Some(object) = outbound.as_object_mut() {
        object.remove("tag");
    }
    Ok(Node::new(
        name,
        source.to_string(),
        protocol.to_string(),
        server,
        port,
        outbound,
    ))
}

fn attach_transport_tls(proxy: &Mapping, network: &str, outbound: &mut Map<String, Value>) {
    attach_transport(proxy, network, outbound);
    let enabled = tls_enabled(proxy) || yaml_get(proxy, "reality-opts").is_some();
    if enabled || network == "ws" || network == "grpc" {
        outbound.insert("tls".into(), tls_value(proxy, enabled));
    }
}

fn attach_transport(proxy: &Mapping, network: &str, outbound: &mut Map<String, Value>) {
    match network {
        "ws" => {
            let mut transport = Map::new();
            transport.insert("type".into(), json!("ws"));
            if let Some(opts) = yaml_get(proxy, "ws-opts") {
                if let Some(path) = opts
                    .as_mapping()
                    .and_then(|mapping| yaml_get(mapping, "path"))
                    .and_then(YamlValue::as_str)
                {
                    transport.insert("path".into(), json!(path));
                }
                if let Some(headers) = opts
                    .as_mapping()
                    .and_then(|mapping| yaml_get(mapping, "headers"))
                    .and_then(YamlValue::as_mapping)
                {
                    let mut header_map = Map::new();
                    for (key, value) in headers {
                        if let (Some(key), Some(value)) = (key.as_str(), value.as_str()) {
                            header_map.insert(key.to_string(), json!(value));
                        }
                    }
                    if !header_map.is_empty() {
                        transport.insert("headers".into(), Value::Object(header_map));
                    }
                }
            }
            outbound.insert("transport".into(), Value::Object(transport));
        }
        "grpc" => {
            let mut transport = Map::new();
            transport.insert("type".into(), json!("grpc"));
            if let Some(service_name) = yaml_get(proxy, "grpc-opts")
                .and_then(YamlValue::as_mapping)
                .and_then(|mapping| yaml_get(mapping, "grpc-service-name"))
                .and_then(YamlValue::as_str)
            {
                transport.insert("service_name".into(), json!(service_name));
            }
            outbound.insert("transport".into(), Value::Object(transport));
        }
        "h2" | "http" => {
            let mut transport = Map::new();
            transport.insert("type".into(), json!("http"));
            if let Some(opts) = yaml_get(proxy, "h2-opts") {
                if let Some(host) = opts
                    .as_mapping()
                    .and_then(|mapping| yaml_get(mapping, "host"))
                {
                    transport.insert("host".into(), yaml_to_json(host));
                }
                if let Some(path) = opts
                    .as_mapping()
                    .and_then(|mapping| yaml_get(mapping, "path"))
                    .and_then(YamlValue::as_str)
                {
                    transport.insert("path".into(), json!(path));
                }
            }
            outbound.insert("transport".into(), Value::Object(transport));
        }
        _ => {}
    }
}

fn tls_enabled(proxy: &Mapping) -> bool {
    match yaml_get(proxy, "tls") {
        Some(YamlValue::Bool(value)) => *value,
        Some(YamlValue::String(value)) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "none" | "false" | "0"
        ),
        _ => false,
    }
}

fn ensure_supported_network(network: &str) -> Result<(), String> {
    match network.to_ascii_lowercase().as_str() {
        "tcp" | "ws" | "grpc" | "h2" | "http" => Ok(()),
        other => Err(format!("unsupported transport: {other}")),
    }
}

fn tls_value(proxy: &Mapping, enabled: bool) -> Value {
    let mut tls = Map::new();
    tls.insert("enabled".into(), json!(enabled));
    if let Some(server_name) = yaml_str(proxy, "servername")
        .or_else(|| yaml_str(proxy, "sni"))
        .or_else(|| yaml_str(proxy, "host"))
    {
        tls.insert("server_name".into(), json!(server_name));
    }
    if yaml_bool(proxy, "skip-cert-verify").unwrap_or(false) {
        tls.insert("insecure".into(), json!(true));
    }
    if let Some(alpn) = yaml_get(proxy, "alpn") {
        let values = match alpn {
            YamlValue::String(value) => value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>(),
            YamlValue::Sequence(values) => values
                .iter()
                .filter_map(YamlValue::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        if !values.is_empty() {
            tls.insert("alpn".into(), json!(values));
        }
    }
    if let Some(fingerprint) = yaml_str(proxy, "client-fingerprint") {
        tls.insert(
            "utls".into(),
            json!({ "enabled": true, "fingerprint": fingerprint }),
        );
    }
    if let Some(reality) = yaml_get(proxy, "reality-opts") {
        if let Some(public_key) = reality
            .as_mapping()
            .and_then(|mapping| yaml_get(mapping, "public-key"))
            .and_then(YamlValue::as_str)
        {
            let short_id = reality
                .as_mapping()
                .and_then(|mapping| yaml_get(mapping, "short-id"))
                .and_then(YamlValue::as_str)
                .unwrap_or("");
            tls.insert(
                "reality".into(),
                json!({
                    "enabled": true,
                    "public_key": public_key,
                    "short_id": short_id
                }),
            );
        }
    }
    Value::Object(tls)
}

fn add_auth(proxy: &Mapping, outbound: &mut Map<String, Value>) {
    if let Some(username) = yaml_str(proxy, "username") {
        outbound.insert("username".into(), json!(username));
    }
    if let Some(password) = yaml_str(proxy, "password") {
        outbound.insert("password".into(), json!(password));
    }
}

fn yaml_get<'a>(map: &'a Mapping, key: &str) -> Option<&'a YamlValue> {
    let key = YamlValue::String(key.to_string());
    map.get(&key)
}

fn yaml_str<'a>(map: &'a Mapping, key: &str) -> Option<&'a str> {
    let key = YamlValue::String(key.to_string());
    map.get(&key).and_then(YamlValue::as_str)
}

fn yaml_u16(map: &Mapping, key: &str) -> Option<u16> {
    let key = YamlValue::String(key.to_string());
    map.get(&key)
        .and_then(YamlValue::as_u64)
        .and_then(|value| u16::try_from(value).ok())
}

fn yaml_u64(map: &Mapping, key: &str) -> Option<u64> {
    let key = YamlValue::String(key.to_string());
    map.get(&key).and_then(YamlValue::as_u64)
}

/// Map a Clash duration (seconds as a number, or a Go-style duration string)
/// to sing-box's duration string. Invalid optional values are dropped so a
/// bad subscription field cannot fail `sing-box check` for the whole profile.
fn insert_duration(
    proxy: &Mapping,
    clash_key: &str,
    sing_key: &str,
    outbound: &mut Map<String, Value>,
) {
    let Some(value) = yaml_get(proxy, clash_key) else {
        return;
    };
    let duration = match value {
        YamlValue::Number(number) => number
            .as_u64()
            .filter(|seconds| *seconds > 0)
            .map(|seconds| format!("{seconds}s")),
        YamlValue::String(text) => {
            let text = text.trim();
            is_go_duration(text).then(|| text.to_string())
        }
        _ => None,
    };
    if let Some(duration) = duration {
        outbound.insert(sing_key.to_string(), json!(duration));
    }
}

fn is_go_duration(text: &str) -> bool {
    let mut rest = text;
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        let number = &rest[..digits];
        if digits == 0 || number.ends_with('.') || number.parse::<f64>().is_err() {
            return false;
        }
        rest = &rest[digits..];
        let Some(unit) = ["ns", "us", "µs", "ms", "s", "m", "h"]
            .iter()
            .find(|unit| rest.starts_with(*unit))
        else {
            return false;
        };
        rest = &rest[unit.len()..];
    }
    true
}

fn yaml_bool(map: &Mapping, key: &str) -> Option<bool> {
    let key = YamlValue::String(key.to_string());
    map.get(&key).and_then(YamlValue::as_bool)
}

fn yaml_to_json(value: &YamlValue) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_clash_proxies() {
        let yaml = r#"
proxies:
  - name: "香港 SS"
    type: ss
    server: example.com
    port: 443
    cipher: aes-128-gcm
    password: secret
  - name: "日本 VMess"
    type: vmess
    server: 2001:db8::1
    port: 443
    uuid: 11111111-1111-1111-1111-111111111111
    alterId: 0
    cipher: auto
    tls: true
    network: ws
    servername: cdn.example.com
    ws-opts:
      path: /ws
      headers:
        Host: cdn.example.com
  - name: "美国 Trojan"
    type: trojan
    server: example.net
    port: 443
    password: pass
    sni: example.net
"#;
        let (nodes, unsupported) = parse_clash(yaml, "test").unwrap();
        assert_eq!(unsupported.len(), 0);
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].protocol, "shadowsocks");
        assert_eq!(nodes[1].server, "2001:db8::1");
        assert_eq!(nodes[1].outbound["transport"]["type"], "ws");
        assert_eq!(nodes[1].outbound["tls"]["server_name"], "cdn.example.com");
        assert_eq!(nodes[2].outbound["type"], "trojan");
    }

    #[test]
    fn skips_shadowsocks_plugin() {
        let yaml = r#"
proxies:
  - name: plugin
    type: ss
    server: example.com
    port: 443
    cipher: aes-128-gcm
    password: secret
    plugin: obfs
"#;
        let (nodes, unsupported) = parse_clash(yaml, "test").unwrap();
        assert!(nodes.is_empty());
        assert_eq!(unsupported.len(), 1);
    }

    #[test]
    fn rejects_zero_ports_and_empty_credentials() {
        let yaml = r#"
proxies:
  - name: zero
    type: ss
    server: example.com
    port: 0
    cipher: aes-128-gcm
    password: secret
  - name: empty-password
    type: ss
    server: example.com
    port: 443
    cipher: aes-128-gcm
    password: ""
"#;
        let (nodes, unsupported) = parse_clash(yaml, "test").unwrap();
        assert!(nodes.is_empty());
        assert_eq!(unsupported.len(), 2);
        assert!(unsupported
            .iter()
            .any(|(_, reason)| reason.contains("port")));
        assert!(unsupported
            .iter()
            .any(|(_, reason)| reason.contains("empty")));
    }

    #[test]
    fn reports_unsupported_type() {
        let yaml = r#"
proxies:
  - name: wg
    type: wireguard
    server: example.com
    port: 51820
"#;
        let (nodes, unsupported) = parse_clash(yaml, "test").unwrap();
        assert!(nodes.is_empty());
        assert!(unsupported[0].1.contains("wireguard"));
    }

    #[test]
    fn parses_anytls_with_tls_and_session_tuning() {
        let yaml = r#"
proxies:
  - name: "anytls node"
    type: anytls
    server: example.com
    port: 8443
    password: secret
    sni: cdn.example.com
    alpn: [h2, http/1.1]
    skip-cert-verify: true
    client-fingerprint: chrome
    idle-session-check-interval: 30
    idle-session-timeout: 1m
    min-idle-session: 2
"#;
        let (nodes, unsupported) = parse_clash(yaml, "test").unwrap();
        assert!(unsupported.is_empty());
        assert_eq!(nodes.len(), 1);
        let outbound = &nodes[0].outbound;
        assert_eq!(nodes[0].protocol, "anytls");
        assert_eq!(outbound["type"], "anytls");
        assert_eq!(outbound["server_port"], 8443);
        assert_eq!(outbound["password"], "secret");
        assert_eq!(outbound["tls"]["enabled"], true);
        assert_eq!(outbound["tls"]["server_name"], "cdn.example.com");
        assert_eq!(outbound["tls"]["insecure"], true);
        assert_eq!(outbound["tls"]["utls"]["fingerprint"], "chrome");
        assert_eq!(outbound["idle_session_check_interval"], "30s");
        assert_eq!(outbound["idle_session_timeout"], "1m");
        assert_eq!(outbound["min_idle_session"], 2);
    }

    #[test]
    fn anytls_drops_invalid_duration_and_requires_password() {
        let yaml = r#"
proxies:
  - name: "bad duration"
    type: anytls
    server: example.com
    port: 443
    password: secret
    idle-session-timeout: soon
  - name: "no password"
    type: anytls
    server: example.com
    port: 443
  - name: "empty password"
    type: anytls
    server: example.com
    port: 443
    password: ""
  - name: "camouflage"
    type: anytls
    server: example.com
    port: 443
    password: secret
    shadow-tls-opts:
      version: 3
      password: pass
"#;
        let (nodes, unsupported) = parse_clash(yaml, "test").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].outbound["tls"]["enabled"], true);
        assert!(nodes[0].outbound.get("idle_session_timeout").is_none());
        assert_eq!(unsupported.len(), 3);
        assert!(unsupported[0].1.contains("missing password"));
        assert!(unsupported[1].1.contains("missing password"));
        assert!(unsupported[2].1.contains("camouflage"));
    }

    #[test]
    fn go_duration_validation() {
        for valid in [
            "30s", "1m", "1h30m", "500ms", "1.5s", ".5s", "250us", "100ns",
        ] {
            assert!(is_go_duration(valid), "{valid} should be valid");
        }
        for invalid in ["", "soon", "30", "s", "1d", "-5s", "1x", "1m3", "1.s"] {
            assert!(!is_go_duration(invalid), "{invalid} should be invalid");
        }
    }

    #[test]
    fn maps_http_tls_and_rejects_unknown_transport() {
        let yaml = r#"
proxies:
  - name: secure http
    type: http
    server: example.com
    port: 443
    tls: true
    skip-cert-verify: true
    servername: proxy.example.com
  - name: bad transport
    type: vless
    server: example.com
    port: 443
    uuid: 11111111-1111-1111-1111-111111111111
    network: kcp
"#;
        let (nodes, unsupported) = parse_clash(yaml, "test").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].outbound["tls"]["enabled"], true);
        assert_eq!(nodes[0].outbound["tls"]["insecure"], true);
        assert_eq!(nodes[0].outbound["tls"]["server_name"], "proxy.example.com");
        assert_eq!(unsupported.len(), 1);
        assert!(unsupported[0].1.contains("unsupported transport"));
    }
}
