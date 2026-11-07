use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::subscription::model::Node;

pub fn stable_tag(protocol: &str, server: &str, port: u16, material: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(protocol.as_bytes());
    hasher.update(b"|");
    hasher.update(server.as_bytes());
    hasher.update(b"|");
    hasher.update(port.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(material.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("n-{}", &hex[..16])
}

pub fn node_material(outbound: &Value) -> String {
    serde_json::to_string(outbound).unwrap_or_default()
}

fn node_matches(node: &Node, include: &[String], exclude: &[String]) -> bool {
    let haystack = format!("{} {}", node.source, node.name).to_lowercase();
    let included = include.is_empty()
        || include
            .iter()
            .any(|keyword| haystack.contains(&keyword.to_lowercase()));
    let excluded = exclude
        .iter()
        .any(|keyword| haystack.contains(&keyword.to_lowercase()));
    included && !excluded
}

pub fn filter_nodes(nodes: &[Node], include: &[String], exclude: &[String]) -> Vec<Node> {
    nodes
        .iter()
        .filter(|node| node_matches(node, include, exclude))
        .cloned()
        .collect()
}

/// Same filter as `filter_nodes`, without cloning each node's outbound JSON.
///
/// UI counts, selector prechecks, and sorting only need tags; cloning a full
/// `Node` (including credentials) on every view tick is unnecessary.
pub fn filter_node_tags(nodes: &[Node], include: &[String], exclude: &[String]) -> Vec<String> {
    nodes
        .iter()
        .filter(|node| node_matches(node, include, exclude))
        .map(|node| node.tag.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, source: &str) -> Node {
        Node::new(
            name.to_string(),
            source.to_string(),
            "shadowsocks".to_string(),
            "example.com".to_string(),
            443,
            serde_json::json!({
                "type": "shadowsocks",
                "server": "example.com",
                "server_port": 443,
                "method": "aes-128-gcm",
                "password": "secret"
            }),
        )
    }

    #[test]
    fn tag_is_stable_and_short() {
        let a = stable_tag("ss", "example.com", 443, "material");
        let b = stable_tag("ss", "example.com", 443, "material");
        assert_eq!(a, b);
        assert!(a.starts_with("n-"));
        assert_eq!(a.len(), 18);
        assert_ne!(a, stable_tag("ss", "example.com", 444, "material"));
    }

    #[test]
    fn filter_include_exclude_case_insensitive() {
        let nodes = vec![node("香港 01", "airport"), node("日本 02", "airport")];
        let selected = filter_nodes(&nodes, &["hk".into(), "香港".into()], &[]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "香港 01");

        let selected = filter_nodes(&nodes, &[], &["日本".into()]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "香港 01");
    }

    #[test]
    fn empty_include_selects_all() {
        let nodes = vec![node("A", "s"), node("B", "s")];
        assert_eq!(filter_nodes(&nodes, &[], &[]).len(), 2);
    }

    #[test]
    fn tag_filter_matches_node_filter_without_cloning_nodes() {
        let nodes = vec![node("香港 01", "airport"), node("日本 02", "airport")];
        let include = vec!["香港".to_string()];
        let excluded = Vec::new();
        let cloned = filter_nodes(&nodes, &include, &excluded);
        let tags = filter_node_tags(&nodes, &include, &excluded);
        assert_eq!(
            cloned
                .iter()
                .map(|node| node.tag.clone())
                .collect::<Vec<_>>(),
            tags
        );
    }
}
