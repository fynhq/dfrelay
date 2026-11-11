use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

use crate::selection::{node_material, stable_tag};

/// A parsed outbound kept as compact JSON instead of a `serde_json::Value`
/// tree. The JSON text is immutable and shared between node clones; config
/// generation re-parses it only when a proxy start actually needs it.
#[derive(Debug, Clone)]
pub struct Outbound(Arc<RawValue>);

impl Outbound {
    pub fn value(&self) -> Value {
        serde_json::from_str(self.0.get()).expect("stored outbound is valid JSON")
    }

    pub fn from_value(value: &Value) -> Self {
        let raw = serde_json::value::to_raw_value(value).expect("serialize outbound");
        Self(Arc::from(raw))
    }
}

impl PartialEq for Outbound {
    fn eq(&self, other: &Self) -> bool {
        self.0.get() == other.0.get()
    }
}

impl Serialize for Outbound {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Outbound {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Ok(Self(Arc::from(raw)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub tag: String,
    pub name: String,
    pub source: String,
    pub protocol: String,
    pub server: String,
    pub port: u16,
    /// Compact, immutable outbound JSON. Sharing it through `Arc` keeps
    /// per-source runtimes, the merged node list, cache snapshots and filtered
    /// candidate lists from each copying the full object.
    pub outbound: Outbound,
}

impl Node {
    pub fn new(
        name: String,
        source: String,
        protocol: String,
        server: String,
        port: u16,
        mut outbound: Value,
    ) -> Self {
        let material = node_material(&outbound);
        let tag = stable_tag(&protocol, &server, port, &material);
        if let Some(object) = outbound.as_object_mut() {
            object.insert("tag".to_string(), Value::String(tag.clone()));
        }
        Self {
            tag,
            name: crate::text::sanitize_owned(name),
            source: crate::text::sanitize_owned(source),
            protocol,
            server,
            port,
            outbound: Outbound::from_value(&outbound),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node() -> Node {
        Node::new(
            "node".to_string(),
            "airport".to_string(),
            "shadowsocks".to_string(),
            "example.com".to_string(),
            443,
            json!({
                "type": "shadowsocks",
                "server": "example.com",
                "server_port": 443,
                "method": "aes-128-gcm",
                "password": "secret"
            }),
        )
    }

    #[test]
    fn clones_share_the_immutable_outbound() {
        let original = node();
        let cloned = original.clone();
        assert!(Arc::ptr_eq(&original.outbound.0, &cloned.outbound.0));
        assert_eq!(original.outbound.value()["method"], "aes-128-gcm");
    }
}
