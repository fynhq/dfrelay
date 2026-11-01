use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::SubscriptionConfig;
use crate::fsutil;

use super::detect::UnsupportedNode;
use super::model::Node;
use super::SubscriptionRuntime;

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    subscriptions: Vec<CachedSubscription>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedSubscription {
    id: String,
    url_hash: String,
    #[serde(default)]
    last_updated: Option<String>,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    unsupported: Vec<UnsupportedNode>,
}

pub fn load(path: &Path, configs: &[SubscriptionConfig]) -> Vec<SubscriptionRuntime> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(cache) = serde_json::from_str::<CacheFile>(&text) else {
        return Vec::new();
    };
    if cache.version != CACHE_VERSION {
        return Vec::new();
    }
    configs
        .iter()
        .filter(|config| config.enabled)
        .filter_map(|config| {
            let cached = cache
                .subscriptions
                .iter()
                .find(|item| item.id == config.id && item.url_hash == url_hash(&config.url))?;
            if cached.nodes.is_empty() {
                return None;
            }
            Some(SubscriptionRuntime {
                config: config.clone(),
                nodes: cached.nodes.clone(),
                unsupported: cached.unsupported.clone(),
                last_updated: cached.last_updated.clone(),
                error: None,
                stale: true,
            })
        })
        .collect()
}

pub fn save(path: &Path, runtimes: &[SubscriptionRuntime]) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fsutil::ensure_private_dir(parent)?;
    }
    let cache = CacheFile {
        version: CACHE_VERSION,
        subscriptions: runtimes
            .iter()
            .map(|runtime| CachedSubscription {
                id: runtime.config.id.clone(),
                url_hash: url_hash(&runtime.config.url),
                last_updated: runtime.last_updated.clone(),
                nodes: runtime.nodes.clone(),
                unsupported: runtime.unsupported.clone(),
            })
            .collect(),
    };
    let text = serde_json::to_string(&cache).map_err(std::io::Error::other)?;
    fsutil::write_private_atomic(path, text.as_bytes())
}

fn url_hash(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn config(id: &str, url: &str) -> SubscriptionConfig {
        SubscriptionConfig {
            id: id.to_string(),
            name: "airport".to_string(),
            url: url.to_string(),
            enabled: true,
        }
    }

    fn runtime(config: SubscriptionConfig) -> SubscriptionRuntime {
        let node = Node::new(
            "node".to_string(),
            "airport".to_string(),
            "shadowsocks".to_string(),
            "example.com".to_string(),
            443,
            json!({
                "type": "shadowsocks", "server": "example.com", "server_port": 443,
                "method": "aes-128-gcm", "password": "secret"
            }),
        );
        SubscriptionRuntime {
            config,
            nodes: vec![node],
            unsupported: Vec::new(),
            last_updated: Some("2026-10-29 10:00:00".to_string()),
            error: None,
            stale: false,
        }
    }

    #[test]
    fn roundtrip_keeps_nodes_and_marks_stale_until_refresh() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cache.json");
        let config = config("one", "https://example.com/sub?token=secret");
        save(&path, &[runtime(config.clone())]).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("token=secret"));

        let loaded = load(&path, &[config]);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].nodes.len(), 1);
        assert!(loaded[0].stale);
        assert!(loaded[0].last_updated.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn cache_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cache.json");
        save(&path, &[runtime(config("one", "https://example.com/sub"))]).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn url_change_invalidates_cached_nodes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cache.json");
        save(&path, &[runtime(config("one", "https://example.com/old"))]).unwrap();
        assert!(load(&path, &[config("one", "https://example.com/new")]).is_empty());
    }

    #[test]
    fn disabled_or_unknown_subscriptions_are_not_loaded() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cache.json");
        let mut disabled = config("one", "https://example.com/sub");
        disabled.enabled = false;
        save(&path, &[runtime(config("one", "https://example.com/sub"))]).unwrap();
        assert!(load(&path, &[disabled]).is_empty());
        assert!(load(&path, &[config("other", "https://example.com/sub")]).is_empty());
    }
}
