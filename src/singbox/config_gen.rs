use std::collections::HashSet;
use std::path::Path;

use serde_json::{json, Value};

use crate::config::{Config, DEFAULT_DNS_SERVERS};
use crate::logging::LogLevel;
use crate::selection::filter_nodes;
use crate::subscription::model::Node;

#[derive(Debug, thiserror::Error)]
pub enum ConfigGenError {
    #[error("no nodes available")]
    NoNodes,
    #[error("no nodes match the keyword whitelist")]
    NoMatchingNodes,
    #[error("duplicate node tag: {0}")]
    DuplicateTag(String),
    #[error("clash api secret must not be empty")]
    EmptySecret,
}

#[cfg(test)]
pub fn generate(
    config: &Config,
    nodes: &[Node],
    ipv6_available: bool,
    clash_port: u16,
    clash_secret: &str,
    cache_path: &Path,
) -> Result<Value, ConfigGenError> {
    generate_with_selection(
        config,
        nodes,
        ipv6_available,
        clash_port,
        clash_secret,
        cache_path,
        None,
    )
}

pub fn generate_with_selection(
    config: &Config,
    nodes: &[Node],
    ipv6_available: bool,
    clash_port: u16,
    clash_secret: &str,
    cache_path: &Path,
    preferred: Option<&str>,
) -> Result<Value, ConfigGenError> {
    if nodes.is_empty() {
        return Err(ConfigGenError::NoNodes);
    }
    if clash_secret.is_empty() {
        return Err(ConfigGenError::EmptySecret);
    }

    let mut seen = HashSet::new();
    for node in nodes {
        if !seen.insert(node.tag.clone()) {
            return Err(ConfigGenError::DuplicateTag(node.tag.clone()));
        }
    }

    let listen_v4 = if config.allow_lan {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    };
    let lan_auth = config.lan_auth();
    let mut inbound_v4 = json!({
        "type": "mixed",
        "tag": "mixed-v4",
        "listen": listen_v4,
        "listen_port": config.port
    });
    if let Some((username, password)) = lan_auth {
        inbound_v4["users"] = json!([{ "username": username, "password": password }]);
    }
    let mut inbounds = vec![inbound_v4];
    if config.enable_ipv6 && ipv6_available {
        let mut inbound_v6 = json!({
            "type": "mixed",
            "tag": "mixed-v6",
            "listen": "::1",
            "listen_port": config.port
        });
        if let Some((username, password)) = lan_auth {
            inbound_v6["users"] = json!([{ "username": username, "password": password }]);
        }
        inbounds.push(inbound_v6);
    }

    let node_tags: Vec<String> = nodes.iter().map(|node| node.tag.clone()).collect();
    let auto_nodes = filter_nodes(nodes, &config.keywords_include, &config.keywords_exclude);
    if auto_nodes.is_empty() {
        return Err(ConfigGenError::NoMatchingNodes);
    }
    let auto_tags: Vec<String> = auto_nodes.iter().map(|node| node.tag.clone()).collect();
    let selector_outbounds = if auto_tags.len() >= 2 {
        let mut values = vec!["auto".to_string()];
        values.extend(node_tags.iter().cloned());
        values
    } else {
        node_tags.clone()
    };
    let preferred = preferred.filter(|tag| node_tags.iter().any(|item| item == tag));
    let selector_default = if let Some(tag) = preferred {
        tag.to_string()
    } else if auto_tags.len() >= 2 {
        "auto".to_string()
    } else {
        auto_tags[0].clone()
    };

    let mut outbounds = vec![json!({
        "type": "selector",
        "tag": "proxy",
        "outbounds": selector_outbounds,
        "default": selector_default
    })];

    if auto_tags.len() >= 2 {
        outbounds.push(json!({
            "type": "urltest",
            "tag": "auto",
            "outbounds": auto_tags,
            "url": config.test_url,
            "interval": config.test_interval,
            "tolerance": config.tolerance_ms,
            "idle_timeout": config.idle_timeout
        }));
    }

    outbounds.push(json!({ "type": "direct", "tag": "direct" }));
    for node in nodes {
        outbounds.push(node.outbound.clone());
    }

    let servers = if config.dns_servers.is_empty() {
        DEFAULT_DNS_SERVERS
            .iter()
            .map(|server| server.to_string())
            .collect::<Vec<_>>()
    } else {
        config.dns_servers.clone()
    };
    let dns_servers: Vec<Value> = servers
        .iter()
        .enumerate()
        .map(|(index, server)| {
            json!({
                "type": "udp",
                "tag": format!("dns-{}", index + 1),
                "server": server,
                "server_port": 53
            })
        })
        .collect();
    let dns = json!({
        "servers": dns_servers,
        "final": "dns-1",
        "strategy": "prefer_ipv4"
    });

    let mut route = json!({
        "final": "proxy",
        "default_domain_resolver": "dns-1"
    });
    if config.bypass_lan {
        route["rules"] = json!([
            {
                "domain_suffix": [".local", ".lan", ".home.arpa"],
                "domain": ["localhost"],
                "action": "route",
                "outbound": "direct"
            },
            {
                "ip_is_private": true,
                "action": "route",
                "outbound": "direct"
            }
        ]);
    }

    Ok(json!({
        "log": {
            "level": level_name(config.singbox_log_level),
            "timestamp": true
        },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "dns": dns,
        "route": route,
        "experimental": {
            "cache_file": {
                "enabled": true,
                "path": cache_path.to_string_lossy()
            },
            "clash_api": {
                "external_controller": format!("127.0.0.1:{clash_port}"),
                "secret": clash_secret
            }
        }
    }))
}

fn level_name(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;
    use serde_json::json;

    fn node(name: &str, tag_material: &str) -> Node {
        Node::new(
            name.to_string(),
            "test".to_string(),
            "shadowsocks".to_string(),
            "example.com".to_string(),
            443,
            json!({
                "type": "shadowsocks",
                "server": "example.com",
                "server_port": 443,
                "method": "aes-128-gcm",
                "password": tag_material
            }),
        )
    }

    fn generate_for(nodes: &[Node], ipv6: bool, bypass: bool) -> Value {
        let mut config = Config::default();
        config.port = 12000;
        config.bypass_lan = bypass;
        generate(
            &config,
            nodes,
            ipv6,
            12345,
            "secret",
            Path::new("/tmp/dfrelay-cache.db"),
        )
        .unwrap()
    }

    #[test]
    fn zero_nodes_is_error() {
        let config = Config::default();
        let err =
            generate(&config, &[], true, 1, "secret", Path::new("/tmp/cache.db")).unwrap_err();
        assert!(matches!(err, ConfigGenError::NoNodes));
    }

    #[test]
    fn one_node_has_no_urltest() {
        let value = generate_for(&[node("A", "a")], true, true);
        let outbounds = value["outbounds"].as_array().unwrap();
        assert!(!outbounds.iter().any(|o| o["type"] == "urltest"));
        let selector = outbounds.iter().find(|o| o["type"] == "selector").unwrap();
        assert_eq!(selector["default"], value["outbounds"][2]["tag"]);
        assert_eq!(selector["outbounds"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn two_nodes_have_selector_and_urltest() {
        let value = generate_for(&[node("A", "a"), node("B", "b")], true, true);
        let outbounds = value["outbounds"].as_array().unwrap();
        let selector = outbounds.iter().find(|o| o["type"] == "selector").unwrap();
        let urltest = outbounds.iter().find(|o| o["type"] == "urltest").unwrap();
        assert_eq!(selector["default"], "auto");
        assert_eq!(selector["outbounds"].as_array().unwrap().len(), 3);
        assert_eq!(urltest["outbounds"].as_array().unwrap().len(), 2);
        assert_eq!(urltest["tolerance"], 50);
    }

    #[test]
    fn dual_stack_and_bypass_rules() {
        let value = generate_for(&[node("A", "a")], true, true);
        let inbounds = value["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 2);
        assert_eq!(inbounds[0]["listen"], "127.0.0.1");
        assert_eq!(inbounds[1]["listen"], "::1");
        assert_eq!(value["route"]["rules"].as_array().unwrap().len(), 2);
        assert!(value["route"]["rules"][1]["ip_is_private"]
            .as_bool()
            .unwrap());
    }

    #[test]
    fn allow_lan_binds_ipv4_wildcard_only() {
        let mut config = Config::default();
        config.allow_lan = true;
        let value = generate(
            &config,
            &[node("A", "a")],
            true,
            12345,
            "secret",
            Path::new("/tmp/dfrelay-cache.db"),
        )
        .unwrap();
        let inbounds = value["inbounds"].as_array().unwrap();
        assert_eq!(inbounds[0]["listen"], "0.0.0.0");
        assert_eq!(inbounds[1]["listen"], "::1");
    }

    #[test]
    fn lan_auth_adds_users_only_in_lan_mode() {
        let mut config = Config::default();
        config.allow_lan = true;
        config.lan_username = "alice".to_string();
        config.lan_password = "s3cret".to_string();
        let value = generate(
            &config,
            &[node("A", "a")],
            true,
            12345,
            "secret",
            Path::new("/tmp/dfrelay-cache.db"),
        )
        .unwrap();
        let inbounds = value["inbounds"].as_array().unwrap();
        assert_eq!(inbounds[0]["users"][0]["username"], "alice");
        assert_eq!(inbounds[0]["users"][0]["password"], "s3cret");
        assert_eq!(inbounds[1]["users"][0]["username"], "alice");

        config.allow_lan = false;
        let value = generate(
            &config,
            &[node("A", "a")],
            true,
            12345,
            "secret",
            Path::new("/tmp/dfrelay-cache.db"),
        )
        .unwrap();
        assert!(value["inbounds"][0]["users"].is_null());

        config.allow_lan = true;
        config.lan_password.clear();
        let value = generate(
            &config,
            &[node("A", "a")],
            true,
            12345,
            "secret",
            Path::new("/tmp/dfrelay-cache.db"),
        )
        .unwrap();
        assert!(value["inbounds"][0]["users"].is_null());
    }

    #[test]
    fn ipv6_and_bypass_can_be_disabled() {
        let value = generate_for(&[node("A", "a")], false, false);
        assert_eq!(value["inbounds"].as_array().unwrap().len(), 1);
        assert!(value["route"]["rules"].is_null());
        assert_eq!(value["route"]["final"], "proxy");
    }

    #[test]
    fn clash_api_and_cache_are_configured() {
        let value = generate_for(&[node("A", "a")], true, true);
        assert_eq!(
            value["experimental"]["clash_api"]["external_controller"],
            "127.0.0.1:12345"
        );
        assert_eq!(value["experimental"]["clash_api"]["secret"], "secret");
        assert_eq!(
            value["experimental"]["cache_file"]["path"],
            "/tmp/dfrelay-cache.db"
        );
    }

    #[test]
    fn dns_uses_public_resolvers_as_default_resolver() {
        let value = generate_for(&[node("A", "a")], true, true);
        let servers = value["dns"]["servers"].as_array().unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0]["type"], "udp");
        assert_eq!(servers[0]["server"], "1.1.1.1");
        assert_eq!(servers[1]["server"], "8.8.8.8");
        assert_eq!(value["dns"]["final"], "dns-1");
        assert_eq!(value["route"]["default_domain_resolver"], "dns-1");
    }

    #[test]
    fn duplicate_tags_are_rejected() {
        let a = node("A", "same");
        let b = node("B", "same");
        let mut config = Config::default();
        config.bypass_lan = false;
        let err = generate(
            &config,
            &[a, b],
            true,
            1,
            "secret",
            Path::new("/tmp/cache.db"),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigGenError::DuplicateTag(_)));
    }

    #[test]
    fn keyword_filter_limits_auto_but_selector_keeps_all_nodes() {
        let nodes = vec![
            node("香港 A", "a"),
            node("日本 B", "b"),
            node("美国 C", "c"),
        ];
        let mut config = Config::default();
        config.keywords_include = vec!["香港".into(), "日本".into()];
        let value = generate(
            &config,
            &nodes,
            true,
            1,
            "secret",
            Path::new("/tmp/cache.db"),
        )
        .unwrap();
        let outbounds = value["outbounds"].as_array().unwrap();
        let selector = outbounds.iter().find(|o| o["type"] == "selector").unwrap();
        let urltest = outbounds.iter().find(|o| o["type"] == "urltest").unwrap();
        assert_eq!(selector["outbounds"].as_array().unwrap().len(), 4);
        assert_eq!(urltest["outbounds"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn preferred_node_becomes_selector_default() {
        let nodes = vec![node("香港 A", "a"), node("日本 B", "b")];
        let value = generate_with_selection(
            &Config::default(),
            &nodes,
            true,
            1,
            "secret",
            Path::new("/tmp/cache.db"),
            Some(&nodes[1].tag),
        )
        .unwrap();
        let outbounds = value["outbounds"].as_array().unwrap();
        let selector = outbounds.iter().find(|o| o["type"] == "selector").unwrap();
        let urltest = outbounds.iter().find(|o| o["type"] == "urltest").unwrap();
        assert_eq!(selector["default"], nodes[1].tag);
        assert_eq!(urltest["outbounds"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn missing_preferred_node_falls_back_to_auto() {
        let nodes = vec![node("香港 A", "a"), node("日本 B", "b")];
        let value = generate_with_selection(
            &Config::default(),
            &nodes,
            true,
            1,
            "secret",
            Path::new("/tmp/cache.db"),
            Some("n-missing"),
        )
        .unwrap();
        let outbounds = value["outbounds"].as_array().unwrap();
        let selector = outbounds.iter().find(|o| o["type"] == "selector").unwrap();
        assert_eq!(selector["default"], "auto");
    }

    #[test]
    fn no_keyword_match_is_rejected() {
        let nodes = vec![node("香港 A", "a")];
        let mut config = Config::default();
        config.keywords_include = vec!["不存在".into()];
        let err = generate(
            &config,
            &nodes,
            true,
            1,
            "secret",
            Path::new("/tmp/cache.db"),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigGenError::NoMatchingNodes));
    }
}
