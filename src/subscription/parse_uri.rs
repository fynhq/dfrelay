use std::collections::HashMap;

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::{json, Map, Value};
use url::Url;

use super::model::Node;
use super::outbound::{
    ensure_hysteria2_obfs, ensure_shadowsocks_method, ensure_supported_network,
    ensure_tuic_congestion, ensure_tuic_udp_relay_mode, ensure_vless_flow, ensure_vmess_security,
};

pub type UriParseOutput = (Vec<Node>, Vec<(String, String)>);

pub fn parse_uri_list(text: &str, source: &str) -> Result<UriParseOutput, String> {
    let mut nodes = Vec::new();
    let mut unsupported = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.contains("://") {
            continue;
        }
        match parse_uri(line, source) {
            Ok(node) => {
                if nodes.len() >= super::MAX_NODES_PER_SOURCE {
                    return Err(super::node_limit_error(
                        "nodes",
                        super::MAX_NODES_PER_SOURCE,
                    ));
                }
                nodes.push(node);
            }
            Err(reason) => {
                if unsupported.len() >= super::MAX_UNSUPPORTED_PER_SOURCE {
                    return Err(super::node_limit_error(
                        "unsupported entries",
                        super::MAX_UNSUPPORTED_PER_SOURCE,
                    ));
                }
                unsupported.push((short_name(line), reason));
            }
        }
    }
    Ok((nodes, unsupported))
}

pub fn parse_uri(uri: &str, source: &str) -> Result<Node, String> {
    let scheme = uri
        .split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
        .ok_or_else(|| "not a URI".to_string())?;
    match scheme.as_str() {
        "ss" => parse_ss(uri, source),
        "vmess" => parse_vmess(uri, source),
        "vless" => parse_vless(uri, source),
        "trojan" => parse_trojan(uri, source),
        "hysteria2" | "hy2" => parse_hysteria2(uri, source),
        "tuic" => parse_tuic(uri, source),
        "http" | "https" => parse_http(uri, source),
        "socks5" | "socks" => parse_socks(uri, source),
        other => Err(format!("unsupported protocol: {other}")),
    }
}

pub(crate) fn decode_base64_text(input: &str) -> Option<String> {
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return None;
    }
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(&cleaned) {
            if let Ok(text) = String::from_utf8(bytes) {
                return Some(text);
            }
        }
    }
    None
}

fn strip_uri_scheme<'a>(uri: &'a str, expected: &str) -> Result<&'a str, String> {
    let (scheme, rest) = uri
        .split_once("://")
        .ok_or_else(|| format!("invalid {expected} URI"))?;
    if scheme.eq_ignore_ascii_case(expected) {
        Ok(rest)
    } else {
        Err(format!("invalid {expected} URI"))
    }
}

fn parse_ss(uri: &str, source: &str) -> Result<Node, String> {
    let rest = strip_uri_scheme(uri, "ss")?;
    let (rest, name) = split_fragment(rest);
    let name = name.map(|name| decode_percent(&name));
    let (rest, query) = split_query(rest);
    if query.split('&').any(|part| part.starts_with("plugin=")) {
        return Err("shadowsocks plugin is not supported".to_string());
    }

    let (userinfo, hostport) = if let Some((userinfo, hostport)) = rest.rsplit_once('@') {
        (userinfo.to_string(), hostport.to_string())
    } else {
        let decoded = decode_base64_text(rest).ok_or("invalid ss base64 payload")?;
        let (userinfo, hostport) = decoded
            .rsplit_once('@')
            .ok_or("invalid ss payload: missing host")?;
        (userinfo.to_string(), hostport.to_string())
    };

    let decoded_userinfo = if userinfo.contains(':') {
        decode_percent(&userinfo)
    } else {
        // SIP002 percent-encodes the separator (`method%3Apassword`); try the
        // decoded form before falling back to the legacy base64 form.
        let percent = decode_percent(&userinfo);
        if percent.contains(':') {
            percent
        } else {
            decode_base64_text(&userinfo).ok_or("invalid ss userinfo")?
        }
    };
    let (method, password) = decoded_userinfo
        .split_once(':')
        .ok_or("invalid ss userinfo: expected method:password")?;
    if method.is_empty() || password.is_empty() {
        return Err("ss method or password is empty".to_string());
    }
    ensure_shadowsocks_method(method)?;
    let (server, port) = parse_host_port(&hostport)?;
    if server.is_empty() {
        return Err("ss missing server".to_string());
    }
    let display_name = name.unwrap_or_else(|| format!("{server}:{port}"));
    let outbound = json!({
        "type": "shadowsocks",
        "server": server,
        "server_port": port,
        "method": method,
        "password": password
    });
    Ok(Node::new(
        display_name,
        source.to_string(),
        "shadowsocks".to_string(),
        server,
        port,
        outbound,
    ))
}

fn parse_vmess(uri: &str, source: &str) -> Result<Node, String> {
    let rest = strip_uri_scheme(uri, "vmess")?;
    let decoded = decode_base64_text(rest).ok_or("invalid vmess base64 payload")?;
    let value: Value =
        serde_json::from_str(&decoded).map_err(|e| format!("invalid vmess JSON: {e}"))?;
    let object = value.as_object().ok_or("vmess payload is not an object")?;

    let server = normalize_host(string_field(object, "add").ok_or("vmess missing add")?);
    if server.is_empty() {
        return Err("vmess missing add".to_string());
    }
    let port = number_field(object, "port").ok_or("vmess missing port")?;
    if port == 0 {
        return Err("vmess invalid port".to_string());
    }
    let uuid = string_field(object, "id").ok_or("vmess missing id")?;
    if uuid.is_empty() {
        return Err("vmess missing id".to_string());
    }
    let name = string_field(object, "ps")
        .map(str::to_string)
        .unwrap_or_else(|| format!("{server}:{port}"));
    let alter_id = number_field(object, "aid").unwrap_or(0);
    let security = string_field(object, "scy")
        .filter(|value| !value.is_empty())
        .unwrap_or("auto");
    ensure_vmess_security(security)?;
    let network = string_field(object, "net")
        .filter(|network| !network.is_empty())
        .unwrap_or("tcp");
    ensure_supported_network(network)?;
    let tls_enabled = tls_string_enabled(string_field(object, "tls"));
    let sni = string_field(object, "sni").filter(|s| !s.is_empty());
    let host = string_field(object, "host").filter(|s| !s.is_empty());
    let path = string_field(object, "path").filter(|s| !s.is_empty());

    let mut outbound = Map::new();
    outbound.insert("type".into(), json!("vmess"));
    outbound.insert("server".into(), json!(server));
    outbound.insert("server_port".into(), json!(port));
    outbound.insert("uuid".into(), json!(uuid));
    outbound.insert("security".into(), json!(security));
    if alter_id > 0 {
        outbound.insert("alter_id".into(), json!(alter_id));
    }
    if let Some(transport) = transport_value(network, host, path, None) {
        outbound.insert("transport".into(), transport);
    }
    if tls_enabled {
        outbound.insert(
            "tls".into(),
            tls_value(true, sni.or(host), false, &[], None, None),
        );
    }

    Ok(Node::new(
        name,
        source.to_string(),
        "vmess".to_string(),
        server.to_string(),
        port,
        Value::Object(outbound),
    ))
}

fn parse_vless(uri: &str, source: &str) -> Result<Node, String> {
    let url = Url::parse(uri).map_err(|e| format!("invalid vless URI: {e}"))?;
    let uuid = decode_percent(url.username());
    if uuid.is_empty() {
        return Err("vless missing uuid".to_string());
    }
    let server = normalize_host(url.host_str().ok_or("vless missing host")?);
    let port = port_or(url.port(), 443)?;
    let query = query_map(&url);
    let network = query
        .get("type")
        .map(String::as_str)
        .filter(|network| !network.is_empty())
        .unwrap_or("tcp");
    ensure_supported_network(network)?;
    let security = query.get("security").map(String::as_str).unwrap_or("");
    let tls_enabled = match security.to_ascii_lowercase().as_str() {
        "" | "none" => false,
        "tls" | "reality" | "xtls" => true,
        other => return Err(format!("unsupported vless security: {other}")),
    };
    let sni = query
        .get("sni")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let host = query
        .get("host")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let path = query
        .get("path")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let service_name = query
        .get("serviceName")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let flow = query
        .get("flow")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    if let Some(flow) = flow {
        ensure_vless_flow(flow)?;
    }
    let insecure = bool_query(&query, "allowInsecure");
    let alpn = split_alpn(query.get("alpn"));
    let fingerprint = query
        .get("fp")
        .map(String::as_str)
        .filter(|s| !s.is_empty());

    let reality = if security.eq_ignore_ascii_case("reality") {
        Some((
            query
                .get("pbk")
                .map(String::as_str)
                .ok_or("vless reality missing public key")?,
            query.get("sid").map(String::as_str).unwrap_or(""),
        ))
    } else {
        None
    };

    let mut outbound = Map::new();
    outbound.insert("type".into(), json!("vless"));
    outbound.insert("server".into(), json!(server));
    outbound.insert("server_port".into(), json!(port));
    outbound.insert("uuid".into(), json!(uuid));
    if let Some(flow) = flow {
        outbound.insert("flow".into(), json!(flow));
    }
    if let Some(transport) = transport_value(network, host, path, service_name) {
        outbound.insert("transport".into(), transport);
    }
    if tls_enabled {
        outbound.insert(
            "tls".into(),
            tls_value(true, sni.or(host), insecure, &alpn, fingerprint, reality),
        );
    }
    let name = fragment_name(&url, &server, port);
    Ok(Node::new(
        name,
        source.to_string(),
        "vless".to_string(),
        server,
        port,
        Value::Object(outbound),
    ))
}

fn single_secret(url: &Url) -> String {
    let mut value = url.username().to_string();
    if let Some(extra) = url.password() {
        if !value.is_empty() {
            value.push(':');
        }
        value.push_str(extra);
    }
    decode_percent(&value)
}

fn parse_trojan(uri: &str, source: &str) -> Result<Node, String> {
    let url = Url::parse(uri).map_err(|e| format!("invalid trojan URI: {e}"))?;
    let password = single_secret(&url);
    if password.is_empty() {
        return Err("trojan missing password".to_string());
    }
    let server = normalize_host(url.host_str().ok_or("trojan missing host")?);
    let port = port_or(url.port(), 443)?;
    let query = query_map(&url);
    let network = query
        .get("type")
        .map(String::as_str)
        .filter(|network| !network.is_empty())
        .unwrap_or("tcp");
    ensure_supported_network(network)?;
    let host = query
        .get("host")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let path = query
        .get("path")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let service_name = query
        .get("serviceName")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let sni = query
        .get("sni")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let insecure = bool_query(&query, "allowInsecure");
    let alpn = split_alpn(query.get("alpn"));

    let mut outbound = Map::new();
    outbound.insert("type".into(), json!("trojan"));
    outbound.insert("server".into(), json!(server));
    outbound.insert("server_port".into(), json!(port));
    outbound.insert("password".into(), json!(password));
    if let Some(transport) = transport_value(network, host, path, service_name) {
        outbound.insert("transport".into(), transport);
    }
    outbound.insert(
        "tls".into(),
        tls_value(true, sni.or(host), insecure, &alpn, None, None),
    );

    let name = fragment_name(&url, &server, port);
    Ok(Node::new(
        name,
        source.to_string(),
        "trojan".to_string(),
        server,
        port,
        Value::Object(outbound),
    ))
}

fn parse_hysteria2(uri: &str, source: &str) -> Result<Node, String> {
    let url = Url::parse(uri).map_err(|e| format!("invalid hysteria2 URI: {e}"))?;
    let password = single_secret(&url);
    if password.is_empty() {
        return Err("hysteria2 missing password".to_string());
    }
    let server = normalize_host(url.host_str().ok_or("hysteria2 missing host")?);
    let port = port_or(url.port(), 443)?;
    let query = query_map(&url);
    let sni = query
        .get("sni")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let insecure = bool_query(&query, "insecure") || bool_query(&query, "allowInsecure");
    let alpn = split_alpn(query.get("alpn"));
    let obfs = query
        .get("obfs")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    if let Some(obfs) = obfs {
        ensure_hysteria2_obfs(obfs)?;
    }

    let mut outbound = Map::new();
    outbound.insert("type".into(), json!("hysteria2"));
    outbound.insert("server".into(), json!(server));
    outbound.insert("server_port".into(), json!(port));
    outbound.insert("password".into(), json!(password));
    if let Some(obfs) = obfs {
        let mut obfs_value = Map::new();
        obfs_value.insert("type".into(), json!(obfs));
        if let Some(password) = query.get("obfs-password") {
            obfs_value.insert("password".into(), json!(password));
        }
        outbound.insert("obfs".into(), Value::Object(obfs_value));
    }
    outbound.insert(
        "tls".into(),
        tls_value(true, sni, insecure, &alpn, None, None),
    );

    let name = fragment_name(&url, &server, port);
    Ok(Node::new(
        name,
        source.to_string(),
        "hysteria2".to_string(),
        server,
        port,
        Value::Object(outbound),
    ))
}

fn parse_tuic(uri: &str, source: &str) -> Result<Node, String> {
    let url = Url::parse(uri).map_err(|e| format!("invalid tuic URI: {e}"))?;
    let uuid = decode_percent(url.username());
    let password = url.password().map(decode_percent).unwrap_or_default();
    if uuid.is_empty() || password.is_empty() {
        return Err("tuic missing uuid or password".to_string());
    }
    let server = normalize_host(url.host_str().ok_or("tuic missing host")?);
    let port = port_or(url.port(), 443)?;
    let query = query_map(&url);
    let sni = query
        .get("sni")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let insecure = bool_query(&query, "allowInsecure") || bool_query(&query, "insecure");
    let alpn = split_alpn(query.get("alpn"));

    let mut outbound = Map::new();
    outbound.insert("type".into(), json!("tuic"));
    outbound.insert("server".into(), json!(server));
    outbound.insert("server_port".into(), json!(port));
    outbound.insert("uuid".into(), json!(uuid));
    outbound.insert("password".into(), json!(password));
    if let Some(congestion) = query.get("congestion_control") {
        ensure_tuic_congestion(congestion)?;
        outbound.insert("congestion_control".into(), json!(congestion));
    }
    if let Some(mode) = query.get("udp_relay_mode") {
        ensure_tuic_udp_relay_mode(mode)?;
        outbound.insert("udp_relay_mode".into(), json!(mode));
    }
    outbound.insert(
        "tls".into(),
        tls_value(true, sni, insecure, &alpn, None, None),
    );

    let name = fragment_name(&url, &server, port);
    Ok(Node::new(
        name,
        source.to_string(),
        "tuic".to_string(),
        server,
        port,
        Value::Object(outbound),
    ))
}

fn parse_http(uri: &str, source: &str) -> Result<Node, String> {
    let url = Url::parse(uri).map_err(|e| format!("invalid http URI: {e}"))?;
    let tls = url.scheme().eq_ignore_ascii_case("https");
    let server = normalize_host(url.host_str().ok_or("http missing host")?);
    let port = port_or(url.port(), if tls { 443 } else { 8080 })?;
    let query = query_map(&url);
    let sni = query
        .get("sni")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    let insecure = bool_query(&query, "insecure") || bool_query(&query, "allowInsecure");
    let alpn = split_alpn(query.get("alpn"));
    let mut outbound = Map::new();
    outbound.insert("type".into(), json!("http"));
    outbound.insert("server".into(), json!(server));
    outbound.insert("server_port".into(), json!(port));
    let username = decode_percent(url.username());
    if !username.is_empty() {
        outbound.insert("username".into(), json!(username));
        if let Some(password) = url.password() {
            outbound.insert("password".into(), json!(decode_percent(password)));
        }
    }
    if tls {
        outbound.insert(
            "tls".into(),
            tls_value(true, sni, insecure, &alpn, None, None),
        );
    }
    let name = fragment_name(&url, &server, port);
    Ok(Node::new(
        name,
        source.to_string(),
        "http".to_string(),
        server,
        port,
        Value::Object(outbound),
    ))
}

fn parse_socks(uri: &str, source: &str) -> Result<Node, String> {
    let url = Url::parse(uri).map_err(|e| format!("invalid socks URI: {e}"))?;
    let server = normalize_host(url.host_str().ok_or("socks missing host")?);
    let port = port_or(url.port(), 1080)?;
    let mut outbound = Map::new();
    outbound.insert("type".into(), json!("socks"));
    outbound.insert("server".into(), json!(server));
    outbound.insert("server_port".into(), json!(port));
    let username = decode_percent(url.username());
    if !username.is_empty() {
        outbound.insert("username".into(), json!(username));
        if let Some(password) = url.password() {
            outbound.insert("password".into(), json!(decode_percent(password)));
        }
    }
    let name = fragment_name(&url, &server, port);
    Ok(Node::new(
        name,
        source.to_string(),
        "socks".to_string(),
        server,
        port,
        Value::Object(outbound),
    ))
}

fn split_fragment(input: &str) -> (&str, Option<String>) {
    match input.split_once('#') {
        Some((body, fragment)) => (body, Some(fragment.to_string())),
        None => (input, None),
    }
}

fn split_query(input: &str) -> (&str, &str) {
    match input.split_once('?') {
        Some((body, query)) => (body, query),
        None => (input, ""),
    }
}

fn parse_host_port(input: &str) -> Result<(String, u16), String> {
    if let Some(rest) = input.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or("invalid IPv6 host")?;
        let port = tail
            .strip_prefix(':')
            .ok_or("missing port")?
            .parse::<u16>()
            .map_err(|_| "invalid port".to_string())?;
        if port == 0 {
            return Err("invalid port".to_string());
        }
        return Ok((host.to_string(), port));
    }
    let (host, port) = input.rsplit_once(':').ok_or("missing port")?;
    let port = port
        .parse::<u16>()
        .map_err(|_| "invalid port".to_string())?;
    if port == 0 {
        return Err("invalid port".to_string());
    }
    Ok((host.to_string(), port))
}

fn normalize_host(host: &str) -> String {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .to_string()
}

fn port_or(port: Option<u16>, default: u16) -> Result<u16, String> {
    let port = port.unwrap_or(default);
    if port == 0 {
        Err("invalid port".to_string())
    } else {
        Ok(port)
    }
}

fn decode_percent(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn fragment_name(url: &Url, server: &str, port: u16) -> String {
    url.fragment()
        .filter(|name| !name.is_empty())
        .map(decode_percent)
        .unwrap_or_else(|| format!("{server}:{port}"))
}

fn tls_string_enabled(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "none" | "false" | "0"
    )
}

fn query_map(url: &Url) -> HashMap<String, String> {
    url.query_pairs().into_owned().collect()
}

fn bool_query(query: &HashMap<String, String>, key: &str) -> bool {
    query.get(key).is_some_and(|value| {
        matches!(value.as_str(), "1" | "yes") || value.eq_ignore_ascii_case("true")
    })
}

fn split_alpn(value: Option<&String>) -> Vec<String> {
    value
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn transport_value(
    network: &str,
    host: Option<&str>,
    path: Option<&str>,
    service_name: Option<&str>,
) -> Option<Value> {
    match network {
        "ws" => {
            let mut transport = Map::new();
            transport.insert("type".into(), json!("ws"));
            transport.insert("path".into(), json!(path.unwrap_or("/")));
            if let Some(host) = host {
                transport.insert("headers".into(), json!({ "Host": host }));
            }
            Some(Value::Object(transport))
        }
        "grpc" => {
            let mut transport = Map::new();
            transport.insert("type".into(), json!("grpc"));
            transport.insert(
                "service_name".into(),
                json!(service_name.or(path).unwrap_or("")),
            );
            Some(Value::Object(transport))
        }
        "h2" | "http" => {
            let mut transport = Map::new();
            transport.insert("type".into(), json!("http"));
            if let Some(host) = host {
                transport.insert("host".into(), json!([host]));
            }
            transport.insert("path".into(), json!(path.unwrap_or("/")));
            Some(Value::Object(transport))
        }
        _ => None,
    }
}

fn tls_value(
    enabled: bool,
    server_name: Option<&str>,
    insecure: bool,
    alpn: &[String],
    fingerprint: Option<&str>,
    reality: Option<(&str, &str)>,
) -> Value {
    super::outbound::tls_value(
        enabled,
        server_name,
        Some(insecure),
        alpn,
        fingerprint,
        reality,
    )
}

fn string_field<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn number_field(object: &Map<String, Value>, key: &str) -> Option<u16> {
    object
        .get(key)
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .and_then(|value| u16::try_from(value).ok())
}

fn short_name(uri: &str) -> String {
    let (scheme, rest) = uri
        .split_once("://")
        .map_or(("unknown", ""), |(scheme, rest)| (scheme, rest));
    // VMess payloads are base64 JSON; the base64 is not a host and may encode
    // the UUID. Legacy Shadowsocks payloads have the same problem. Keep only
    // the scheme so an unsupported-node name cannot carry credentials into the
    // cache or an error path.
    if scheme.eq_ignore_ascii_case("vmess") {
        return "vmess://…".to_string();
    }
    if scheme.eq_ignore_ascii_case("ss") {
        let authority = rest.split(['?', '#']).next().unwrap_or(rest);
        if !authority.contains('@') {
            return "ss://…".to_string();
        }
    }
    let host = Url::parse(uri)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string));
    match host {
        Some(host) => format!("{scheme}://{host}"),
        None => format!("{scheme}://…"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ss_sip002() {
        let uri = "ss://YWVzLTEyOC1nY206c2VjcmV0@example.com:443#%E9%A6%99%E6%B8%AF";
        let node = parse_uri(uri, "test").unwrap();
        assert_eq!(node.protocol, "shadowsocks");
        assert_eq!(node.server, "example.com");
        assert_eq!(node.port, 443);
        assert_eq!(node.name, "香港");
        assert_eq!(node.outbound["method"], "aes-128-gcm");
    }

    #[test]
    fn decodes_credentials_and_names() {
        let node = parse_uri("trojan://p%40ss@example.com:443#%E6%97%A5%E6%9C%AC", "test").unwrap();
        assert_eq!(node.outbound["password"], "p@ss");
        assert_eq!(node.name, "日本");

        let proxy = parse_uri("https://us%40er:p%20w@example.com#https", "test").unwrap();
        assert_eq!(proxy.outbound["username"], "us@er");
        assert_eq!(proxy.outbound["password"], "p w");
        assert_eq!(proxy.outbound["server_port"], 443);
        assert_eq!(proxy.outbound["tls"]["enabled"], true);
        assert_eq!(proxy.name, "https");
    }

    #[test]
    fn treats_none_security_and_transport_errors_explicitly() {
        let plain = "vless://11111111-1111-1111-1111-111111111111@example.com:443?security=none#v";
        let node = parse_uri(plain, "test").unwrap();
        assert!(node.outbound.get("tls").is_none());

        let tls = "vless://11111111-1111-1111-1111-111111111111@example.com:443?security=tls&sni=example.com#v";
        assert_eq!(
            parse_uri(tls, "test").unwrap().outbound["tls"]["enabled"],
            true
        );

        let err = parse_uri(
            "vless://11111111-1111-1111-1111-111111111111@example.com:443?type=kcp#v",
            "test",
        )
        .unwrap_err();
        assert!(err.contains("unsupported transport"), "{err}");
    }

    #[test]
    fn vmess_tls_none_stays_plain() {
        let payload = serde_json::json!({
            "v": "2", "ps": "vmess", "add": "example.com", "port": "443",
            "id": "11111111-1111-1111-1111-111111111111", "aid": "0",
            "net": "tcp", "tls": "none"
        });
        let encoded = STANDARD.encode(payload.to_string());
        let node = parse_uri(&format!("vmess://{encoded}"), "test").unwrap();
        assert!(node.outbound.get("tls").is_none());
    }

    #[test]
    fn parses_vmess_json() {
        let payload = serde_json::json!({
            "v": "2", "ps": "vmess", "add": "example.com", "port": "443",
            "id": "11111111-1111-1111-1111-111111111111", "aid": "0",
            "net": "ws", "host": "cdn.example.com", "path": "/ws", "tls": "tls",
            "sni": "cdn.example.com"
        });
        let encoded = STANDARD.encode(payload.to_string());
        let node = parse_uri(&format!("vmess://{encoded}"), "test").unwrap();
        assert_eq!(node.protocol, "vmess");
        assert_eq!(node.outbound["transport"]["type"], "ws");
        assert_eq!(node.outbound["tls"]["server_name"], "cdn.example.com");
    }

    #[test]
    fn parses_vless_reality() {
        let uri = "vless://11111111-1111-1111-1111-111111111111@example.com:443?type=tcp&security=reality&sni=example.com&pbk=abc&sid=1234&fp=chrome#vless";
        let node = parse_uri(uri, "test").unwrap();
        assert_eq!(node.protocol, "vless");
        assert_eq!(node.outbound["tls"]["reality"]["public_key"], "abc");
        assert_eq!(node.outbound["tls"]["utls"]["fingerprint"], "chrome");
    }

    #[test]
    fn accepts_uppercase_scheme_and_keeps_colons_in_single_secret_schemes() {
        let ss = "SS://YWVzLTEyOC1nY206c2VjcmV0@example.com:443#a";
        assert_eq!(parse_uri(ss, "test").unwrap().protocol, "shadowsocks");

        let trojan = parse_uri("trojan://p:ss@example.com:443#t", "test").unwrap();
        assert_eq!(trojan.outbound["password"], "p:ss");

        let hy2 = parse_uri("hysteria2://p:ss@example.com:443#h", "test").unwrap();
        assert_eq!(hy2.outbound["password"], "p:ss");
    }

    #[test]
    fn vmess_normalizes_bracketed_ipv6_and_rejects_zero_ports() {
        let payload = serde_json::json!({
            "v": "2", "ps": "vmess", "add": "[2001:db8::1]", "port": "443",
            "id": "11111111-1111-1111-1111-111111111111", "aid": "0", "net": "tcp"
        });
        let encoded = STANDARD.encode(payload.to_string());
        let node = parse_uri(&format!("vmess://{encoded}"), "test").unwrap();
        assert_eq!(node.server, "2001:db8::1");
        assert_eq!(node.outbound["server"], "2001:db8::1");

        let zero = serde_json::json!({
            "v": "2", "ps": "vmess", "add": "example.com", "port": "0",
            "id": "11111111-1111-1111-1111-111111111111"
        });
        let encoded = STANDARD.encode(zero.to_string());
        assert!(parse_uri(&format!("vmess://{encoded}"), "test").is_err());
    }

    #[test]
    fn rejects_zero_port_and_empty_ss_credentials() {
        assert!(parse_uri("trojan://pass@example.com:0#t", "test").is_err());
        assert!(parse_uri("ss://YWVzLTEyOC1nY206@example.com:443#a", "test").is_err());
    }

    #[test]
    fn parses_trojan_and_hysteria2() {
        let trojan = parse_uri("trojan://pass@example.com:443?sni=example.com#t", "test").unwrap();
        assert_eq!(trojan.outbound["type"], "trojan");
        let hy2 = parse_uri(
            "hysteria2://pass@example.com:443?sni=example.com&obfs=salamander#h",
            "test",
        )
        .unwrap();
        assert_eq!(hy2.outbound["obfs"]["type"], "salamander");
    }

    #[test]
    fn rejects_unsupported_scheme() {
        assert!(parse_uri("wireguard://abc", "test").is_err());
    }

    #[test]
    fn unsupported_names_do_not_expose_userinfo() {
        let (_, unsupported) =
            parse_uri_list("trojan://user:secret@example.com:443?type=quic#bad", "test").unwrap();
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].0, "trojan://example.com");
        assert!(!unsupported[0].0.contains("secret"));
    }

    #[test]
    fn unsupported_base64_names_do_not_expose_credentials() {
        let (_, unsupported) = parse_uri_list(
            "vmess://eyJpZCI6InNlY3JldC11dWlkIn0=\nss://YWVzLTEyOC1nY206c2VjcmV0@example.com:0#bad",
            "test",
        )
        .unwrap();
        assert_eq!(unsupported.len(), 2);
        assert_eq!(unsupported[0].0, "vmess://…");
        assert!(!unsupported[0].0.contains("secret"));
        assert!(!unsupported[1].0.contains("secret"));
    }

    #[test]
    fn rejects_unsupported_protocol_enums() {
        assert!(parse_uri("ss://rc4-md5:secret@example.com:443#a", "test").is_err());
        let vmess = format!(
            "vmess://{}",
            STANDARD.encode(
                r#"{"add":"example.com","port":"443","id":"uuid","net":"tcp","scy":"rc4"}"#
            )
        );
        assert!(parse_uri(&vmess, "test").is_err());
        assert!(parse_uri(
            "vless://11111111-1111-1111-1111-111111111111@example.com:443?flow=xtls-rprx-origin#v",
            "test"
        )
        .is_err());
        assert!(parse_uri("hysteria2://pass@example.com:443?obfs=unknown#h", "test").is_err());
    }

    #[test]
    fn uri_list_skips_comments_and_reports_bad_lines() {
        let text = "# comment\nss://YWVzLTEyOC1nY206c2VjcmV0@example.com:443#a\nwireguard://x\n";
        let (nodes, unsupported) = parse_uri_list(text, "test").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(unsupported.len(), 1);
    }

    #[test]
    fn uri_list_rejects_too_many_nodes() {
        let text: String = (1..=super::super::MAX_NODES_PER_SOURCE + 1)
            .map(|port| format!("ss://YWVzLTEyOC1nY206c2VjcmV0@example.com:{port}#n{port}\n"))
            .collect();
        let err = parse_uri_list(&text, "test").unwrap_err();
        assert!(err.contains("more than"), "{err}");
    }

    #[test]
    fn uri_list_rejects_too_many_unsupported_entries() {
        let text = "wireguard://x\n".repeat(super::super::MAX_UNSUPPORTED_PER_SOURCE + 1);
        let err = parse_uri_list(&text, "test").unwrap_err();
        assert!(err.contains("unsupported entries"), "{err}");
    }
}
