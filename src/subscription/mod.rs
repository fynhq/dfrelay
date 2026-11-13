pub mod cache;
pub mod detect;
pub mod fetch;
pub mod model;
pub mod outbound;
pub mod parse_clash;
pub mod parse_uri;

use std::collections::HashSet;

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::config::SubscriptionConfig;
use detect::{detect_and_parse, UnsupportedNode};
use fetch::fetch_subscription;
pub use fetch::FetchOptions;
use model::Node;

/// Per-source node cap. A subscription body is limited to 10 MiB, but a short
/// URI line can still expand into hundreds of thousands of nodes; bound the
/// parsed result before it reaches config generation.
pub const MAX_NODES_PER_SOURCE: usize = 2000;
/// Per-source unsupported-entry cap: malformed lines allocate names/reasons too.
pub const MAX_UNSUPPORTED_PER_SOURCE: usize = 500;
/// Cap across all enabled subscriptions. Users control the number of sources,
/// so this is a process-memory guard rather than a remote input bound.
pub const MAX_TOTAL_NODES: usize = 5000;

pub(crate) fn node_limit_error(kind: &str, limit: usize) -> String {
    format!("subscription contains more than {limit} {kind}; rejected to bound memory use")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionRuntime {
    pub config: SubscriptionConfig,
    /// URL hash the current node set was fetched from. It can differ from
    /// `config.url` when the user edits a URL and the refresh fails: the old
    /// nodes stay visible, but must not be cached under the new URL.
    #[serde(default)]
    pub source_url_hash: Option<String>,
    pub nodes: Vec<Node>,
    pub unsupported: Vec<UnsupportedNode>,
    pub last_updated: Option<String>,
    pub error: Option<String>,
    pub stale: bool,
}

pub async fn refresh_one(
    config: SubscriptionConfig,
    options: &FetchOptions,
    previous: Option<&SubscriptionRuntime>,
) -> SubscriptionRuntime {
    match fetch_subscription(&config.url, options).await {
        Ok(body) => match detect_and_parse(&body, &config.name) {
            Ok(outcome) if outcome.nodes.is_empty() => {
                let error = if outcome.unsupported.is_empty() {
                    "subscription contains no usable nodes".to_string()
                } else {
                    format!(
                        "no usable nodes ({} unsupported)",
                        outcome.unsupported.len()
                    )
                };
                failed_runtime(config, previous, error)
            }
            Ok(outcome) => SubscriptionRuntime {
                source_url_hash: Some(cache::url_hash(&config.url)),
                config,
                nodes: outcome.nodes,
                unsupported: outcome.unsupported,
                last_updated: Some(Local::now().format("%Y-%m-%d %H:%M:%S").to_string()),
                error: None,
                stale: false,
            },
            Err(err) => failed_runtime(config, previous, err),
        },
        Err(err) => failed_runtime(config, previous, err.to_string()),
    }
}

fn failed_runtime(
    config: SubscriptionConfig,
    previous: Option<&SubscriptionRuntime>,
    error: String,
) -> SubscriptionRuntime {
    match previous.filter(|runtime| !runtime.nodes.is_empty()) {
        Some(previous) => {
            // Keep the source hash of the nodes, not the possibly edited URL.
            let source_url_hash = previous
                .source_url_hash
                .clone()
                .or_else(|| Some(cache::url_hash(&previous.config.url)));
            SubscriptionRuntime {
                config,
                source_url_hash,
                nodes: previous.nodes.clone(),
                unsupported: previous.unsupported.clone(),
                last_updated: previous.last_updated.clone(),
                error: Some(error),
                stale: true,
            }
        }
        None => SubscriptionRuntime {
            config,
            source_url_hash: None,
            nodes: Vec::new(),
            unsupported: Vec::new(),
            last_updated: None,
            error: Some(error),
            stale: false,
        },
    }
}

const REFRESH_CONCURRENCY: usize = 4;

pub async fn refresh_all(
    configs: &[SubscriptionConfig],
    options: &FetchOptions,
    previous: &[SubscriptionRuntime],
) -> Vec<SubscriptionRuntime> {
    let mut results: Vec<Option<SubscriptionRuntime>> = (0..configs.len()).map(|_| None).collect();
    let mut tasks = tokio::task::JoinSet::new();
    let mut next = 0;
    while next < configs.len() || !tasks.is_empty() {
        while next < configs.len() && tasks.len() < REFRESH_CONCURRENCY {
            let config = configs[next].clone();
            let old = previous
                .iter()
                .find(|runtime| runtime.config.id == config.id)
                .cloned();
            let options = options.clone();
            let index = next;
            next += 1;
            tasks.spawn(async move {
                // Keep the subscription index on a panic so its slot is filled
                // with a visible failure instead of silently disappearing.
                let outcome =
                    crate::guard::CatchUnwind::new(refresh_one(config, &options, old.as_ref()))
                        .await;
                match outcome {
                    Ok(runtime) => (index, Ok(runtime)),
                    Err(payload) => (index, Err(crate::guard::panic_message(payload.as_ref()))),
                }
            });
        }
        if let Some(result) = tasks.join_next().await {
            match result {
                Ok((index, Ok(runtime))) => results[index] = Some(runtime),
                Ok((index, Err(message))) => {
                    let config = configs[index].clone();
                    let old = previous
                        .iter()
                        .find(|runtime| runtime.config.id == config.id);
                    results[index] = Some(failed_runtime(
                        config,
                        old,
                        format!("refresh task failed: {message}"),
                    ));
                }
                // Cancellation carries no index; the fill below covers it.
                Err(_) => {}
            }
        }
    }
    // Any slot whose task never reported keeps the previous nodes and records
    // an explicit error so the subscription does not vanish from the list.
    for (index, slot) in results.iter_mut().enumerate() {
        if slot.is_none() {
            let config = configs[index].clone();
            let old = previous
                .iter()
                .find(|runtime| runtime.config.id == config.id);
            *slot = Some(failed_runtime(
                config,
                old,
                "refresh task did not complete".to_string(),
            ));
        }
    }
    results.into_iter().flatten().collect()
}

pub fn merge_nodes(runtimes: &[SubscriptionRuntime]) -> Vec<Node> {
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();
    'runtimes: for runtime in runtimes {
        for node in &runtime.nodes {
            if nodes.len() >= MAX_TOTAL_NODES {
                break 'runtimes;
            }
            if seen.insert(node.tag.clone()) {
                nodes.push(node.clone());
            }
        }
    }
    nodes
}

pub fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let host = parsed.host_str().unwrap_or("invalid");
            format!("{}://{}/***", parsed.scheme(), host)
        }
        Err(_) => "***".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(name: &str, material: &str) -> Node {
        Node::new(
            name.to_string(),
            "test".to_string(),
            "shadowsocks".to_string(),
            "example.com".to_string(),
            443,
            json!({
                "type": "shadowsocks", "server": "example.com", "server_port": 443,
                "method": "aes-128-gcm", "password": material
            }),
        )
    }

    fn runtime(nodes: Vec<Node>) -> SubscriptionRuntime {
        SubscriptionRuntime {
            config: SubscriptionConfig {
                id: "id".into(),
                name: "test".into(),
                url: "https://example.com/sub".into(),
                enabled: true,
            },
            source_url_hash: Some(cache::url_hash("https://example.com/sub")),
            nodes,
            unsupported: Vec::new(),
            last_updated: None,
            error: None,
            stale: false,
        }
    }

    #[test]
    fn merge_deduplicates_stable_tags() {
        let a = node("A", "same");
        let b = node("B", "same");
        let c = node("C", "other");
        let merged = merge_nodes(&[runtime(vec![a, c]), runtime(vec![b])]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_caps_total_nodes() {
        let nodes: Vec<Node> = (0..MAX_TOTAL_NODES + 10)
            .map(|index| node(&format!("n{index}"), &format!("p{index}")))
            .collect();
        let merged = merge_nodes(&[runtime(nodes)]);
        assert_eq!(merged.len(), MAX_TOTAL_NODES);
    }

    #[test]
    fn redacts_subscription_path() {
        assert_eq!(
            redact_url("https://airport.example.com/sub?token=secret"),
            "https://airport.example.com/***"
        );
        assert_eq!(redact_url("not a url"), "***");
    }

    #[test]
    fn failed_refresh_after_url_edit_keeps_the_old_source_hash() {
        let old_url = "https://example.com/old";
        let mut previous = runtime(vec![node("A", "same")]);
        previous.config.url = old_url.to_string();
        previous.source_url_hash = Some(cache::url_hash(old_url));

        let mut new_config = previous.config.clone();
        new_config.url = "https://example.com/new".to_string();
        let failed = failed_runtime(new_config.clone(), Some(&previous), "timeout".to_string());

        assert_eq!(failed.nodes.len(), 1);
        assert_eq!(failed.source_url_hash, Some(cache::url_hash(old_url)));
        assert_ne!(
            failed.source_url_hash,
            Some(cache::url_hash(&new_config.url))
        );
    }

    #[test]
    fn failed_refresh_keeps_previous_nodes_as_stale() {
        let previous = runtime(vec![node("A", "same")]);
        let failed = failed_runtime(
            previous.config.clone(),
            Some(&previous),
            "timeout".to_string(),
        );
        assert_eq!(failed.nodes.len(), 1);
        assert!(failed.stale);
        assert_eq!(failed.error.as_deref(), Some("timeout"));

        let first_failure = failed_runtime(previous.config.clone(), None, "timeout".to_string());
        assert!(first_failure.nodes.is_empty());
        assert!(!first_failure.stale);
    }
}
