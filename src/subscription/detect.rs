use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_yaml_ng::Value as YamlValue;

use super::model::Node;
use super::outbound::{
    ensure_hysteria2_obfs, ensure_shadowsocks_method, ensure_tuic_congestion,
    ensure_tuic_udp_relay_mode, ensure_vless_flow, ensure_vmess_security,
};
use super::parse_clash::parse_clash_document;
use super::parse_uri::{decode_base64_text, parse_uri_list};

/// Maximum number of YAML alias references (`*anchor`) tolerated in a
/// subscription body. Aliases are not needed for a Clash proxy list, and
/// `serde_yaml_ng` expands them by re-parsing anchored subtrees. A single
/// alias can therefore clone a large subtree; reject all aliases instead of
/// trying to guess a safe expansion budget.
const MAX_YAML_ALIASES: usize = 0;

/// Count YAML alias indicators (`*`) that occur at a node boundary.
///
/// This scanner deliberately errs toward counting: a `#` comment or a quoted
/// scalar hides a `*`, and a line break resets the node position. It does not
/// need to be a complete YAML lexer, because a false positive only rejects an
/// exotic subscription while a false negative would let an alias bomb through.
/// In particular, quotes only open a scalar at a node boundary, so an
/// apostrophe inside a plain scalar (`name: Bob's node`) cannot hide later
/// aliases.
fn count_yaml_aliases(text: &str) -> usize {
    let mut count = 0usize;
    let mut at_node_start = true;
    let mut after_space = true;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_comment = false;
    let mut escaped = false;
    for ch in text.chars() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
                at_node_start = true;
                after_space = true;
            }
            continue;
        }
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_double = false;
            }
            continue;
        }
        match ch {
            '\n' => {
                at_node_start = true;
                after_space = true;
            }
            '#' if at_node_start || after_space => {
                in_comment = true;
            }
            '\'' if at_node_start => {
                in_single = true;
                at_node_start = false;
                after_space = false;
            }
            '"' if at_node_start => {
                in_double = true;
                at_node_start = false;
                after_space = false;
            }
            '*' if at_node_start => {
                count += 1;
                at_node_start = false;
                after_space = false;
            }
            '[' | '{' | ',' | '?' => {
                at_node_start = true;
                after_space = true;
            }
            ':' => {
                at_node_start = true;
                after_space = true;
            }
            '-' if at_node_start => {
                at_node_start = true;
                after_space = true;
            }
            ' ' | '\t' => {
                after_space = true;
            }
            _ => {
                at_node_start = false;
                after_space = false;
            }
        }
    }
    count
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn ensure_alias_budget(text: &str) -> Result<(), String> {
    let aliases = count_yaml_aliases(text);
    if aliases > MAX_YAML_ALIASES {
        return Err(format!(
            "subscription uses YAML aliasing ({aliases} aliases); rejected to prevent memory exhaustion"
        ));
    }
    Ok(())
}

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
    if trimmed
        .get(..9)
        .is_some_and(|head| head.eq_ignore_ascii_case("<!doctype"))
        || contains_ascii_case_insensitive(trimmed, "<html")
    {
        return Err("subscription returned an HTML page; check URL or User-Agent".to_string());
    }

    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(outcome) = parse_sing_box_json(&value, source)? {
                return Ok(outcome);
            }
        }
    }

    // Guard against alias-bomb payloads: if the body exceeds the alias budget,
    // skip the YAML parser (which would materialize the expansion) and fall
    // through to the base64/URI parsers. If nothing else matches, surface the
    // aliasing rejection instead of a generic "unrecognized" error.
    let alias_error = ensure_alias_budget(trimmed).err();
    if alias_error.is_none() {
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
    }

    if let Some(decoded) = decode_base64_text(trimmed) {
        if decoded.contains("://") {
            let (nodes, unsupported) = parse_uri_list(&decoded, source)?;
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
        let (nodes, unsupported) = parse_uri_list(trimmed, source)?;
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

    if let Some(message) = alias_error {
        return Err(message);
    }
    Err("unrecognized subscription format".to_string())
}

fn validate_sing_box_node(
    object: &serde_json::Map<String, Value>,
    kind: &str,
) -> Result<(), String> {
    let require = |field: &str| -> Result<&str, String> {
        object
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{kind} missing {field}"))
    };
    match kind {
        "shadowsocks" => {
            let method = require("method")?;
            ensure_shadowsocks_method(method)?;
            require("password")?;
        }
        "vmess" => {
            require("uuid")?;
            if let Some(security) = object
                .get("security")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                ensure_vmess_security(security)?;
            }
        }
        "vless" => {
            require("uuid")?;
            if let Some(flow) = object
                .get("flow")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                ensure_vless_flow(flow)?;
            }
        }
        "trojan" | "hysteria2" | "anytls" => {
            require("password")?;
            if kind == "hysteria2" {
                if let Some(obfs) = object
                    .get("obfs")
                    .and_then(Value::as_object)
                    .and_then(|obfs| obfs.get("type"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    ensure_hysteria2_obfs(obfs)?;
                }
            }
        }
        "tuic" => {
            require("uuid")?;
            require("password")?;
            if let Some(congestion) = object
                .get("congestion_control")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                ensure_tuic_congestion(congestion)?;
            }
            if let Some(mode) = object
                .get("udp_relay_mode")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                ensure_tuic_udp_relay_mode(mode)?;
            }
        }
        "shadowtls" => {
            require("password")?;
        }
        "ssh" => {
            require("user")?;
        }
        "http" | "socks" | "hysteria" | "tor" => {}
        other => return Err(format!("unsupported protocol: {other}")),
    }
    Ok(())
}

fn parse_sing_box_json(value: &Value, source: &str) -> Result<Option<ParseOutcome>, String> {
    let Some(outbounds) = value
        .get("outbounds")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
    else {
        return Ok(None);
    };
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
                if unsupported.len() >= super::MAX_UNSUPPORTED_PER_SOURCE {
                    return Err(super::node_limit_error(
                        "unsupported entries",
                        super::MAX_UNSUPPORTED_PER_SOURCE,
                    ));
                }
                unsupported.push(UnsupportedNode {
                    name,
                    reason: "invalid server_port".to_string(),
                });
                continue;
            }
        };
        if let Err(reason) = validate_sing_box_node(object, kind) {
            if unsupported.len() >= super::MAX_UNSUPPORTED_PER_SOURCE {
                return Err(super::node_limit_error(
                    "unsupported entries",
                    super::MAX_UNSUPPORTED_PER_SOURCE,
                ));
            }
            unsupported.push(UnsupportedNode { name, reason });
            continue;
        }
        if nodes.len() >= super::MAX_NODES_PER_SOURCE {
            return Err(super::node_limit_error(
                "nodes",
                super::MAX_NODES_PER_SOURCE,
            ));
        }
        let mut outbound = outbound.clone();
        if let Some(object) = outbound.as_object_mut() {
            object.remove("tag");
        }
        strip_detour(&mut outbound);
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
        return Ok(None);
    }
    Ok(Some(ParseOutcome { nodes, unsupported }))
}

/// Remove every `detour` reference from a single outbound.
///
/// sing-box accepts `detour` both at the outbound top level and inside `dial`;
/// DFRelay rewrites tags to stable hashes, so any retained reference points at
/// a tag that no longer exists. Dropping it makes one chained node fail alone
/// instead of failing the whole generated configuration.
fn strip_detour(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("detour");
            for nested in object.values_mut() {
                strip_detour(nested);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_detour(item);
            }
        }
        _ => {}
    }
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
    fn strips_nested_dial_detour() {
        let body = serde_json::json!({
            "outbounds": [{
                "type": "vless", "tag": "n1", "server": "example.com",
                "server_port": 443, "uuid": "u",
                "dial": { "detour": "direct", "bind_interface": "eth0" }
            }]
        })
        .to_string();
        let outcome = detect_and_parse(&body, "test").unwrap();
        let outbound = outcome.nodes[0].outbound.value();
        assert!(outbound["dial"].get("detour").is_none());
        assert_eq!(outbound["dial"]["bind_interface"], "eth0");
    }

    #[test]
    fn rejects_unsupported_sing_box_enums() {
        let body = serde_json::json!({
            "outbounds": [{
                "type": "shadowsocks", "tag": "bad", "server": "example.com",
                "server_port": 443, "method": "rc4-md5", "password": "p"
            }]
        })
        .to_string();
        let outcome = detect_and_parse(&body, "test").unwrap();
        assert!(outcome.nodes.is_empty());
        assert_eq!(outcome.unsupported.len(), 1);
        assert!(outcome.unsupported[0].reason.contains("cipher"));
    }

    #[test]
    fn rejects_html() {
        let err = detect_and_parse("<html>login</html>", "test").unwrap_err();
        assert!(err.contains("HTML"));

        let err = detect_and_parse("<!DOCTYPE html><HTML><body>login</body>", "test").unwrap_err();
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

    #[test]
    fn counts_aliases_at_node_boundaries() {
        assert_eq!(count_yaml_aliases("a: *x\nb: *y"), 2);
        assert_eq!(count_yaml_aliases("name: \"a*b*c\""), 0);
        assert_eq!(count_yaml_aliases("name: 'a*b'"), 0);
        assert_eq!(count_yaml_aliases("password: a*b"), 0);
        assert_eq!(count_yaml_aliases("# don't\nb: *x"), 1);
        assert_eq!(count_yaml_aliases("name: Bob's node\nb: *x"), 1);
        assert_eq!(count_yaml_aliases("# ****"), 0);
        assert_eq!(count_yaml_aliases("a: &a [x, x]\nb: [*a]"), 1);
        assert_eq!(count_yaml_aliases(""), 0);
    }

    #[test]
    fn rejects_yaml_alias_bomb() {
        // A billion-laughs style payload: each level aliases the previous one.
        let mut body = String::from("a: &a [x, x, x]\n");
        let mut prev = 'a';
        for level in 0..8 {
            let name = (b'b' + level) as char;
            body.push_str(&format!(
                "{name}: &{name} [*{prev}, *{prev}, *{prev}, *{prev}]\n"
            ));
            prev = name;
        }
        let err = detect_and_parse(&body, "test").unwrap_err();
        assert!(err.contains("aliasing"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_alias_hidden_behind_plain_scalar_apostrophe() {
        let body = "name: Bob's node\nb: *x\n";
        let err = detect_and_parse(body, "test").unwrap_err();
        assert!(err.contains("aliasing"), "unexpected error: {err}");
    }

    #[test]
    fn alias_budget_rejects_any_alias() {
        assert!(ensure_alias_budget("a: x").is_ok());
        assert!(ensure_alias_budget("a: *x").is_err());
        assert!(ensure_alias_budget("name: \"a*b\"").is_ok());
    }

    #[test]
    fn sing_box_json_rejects_too_many_nodes() {
        let outbounds: Vec<Value> = (1..=super::super::MAX_NODES_PER_SOURCE + 1)
            .map(|port| {
                serde_json::json!({
                    "type": "shadowsocks",
                    "tag": format!("n{port}"),
                    "server": "example.com",
                    "server_port": port,
                    "method": "aes-128-gcm",
                    "password": "p"
                })
            })
            .collect();
        let body = serde_json::json!({ "outbounds": outbounds }).to_string();
        let err = detect_and_parse(&body, "test").unwrap_err();
        assert!(err.contains("more than"), "{err}");
    }
}
