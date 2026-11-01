use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::selection::{node_material, stable_tag};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub tag: String,
    pub name: String,
    pub source: String,
    pub protocol: String,
    pub server: String,
    pub port: u16,
    pub outbound: Value,
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
            name,
            source,
            protocol,
            server,
            port,
            outbound,
        }
    }
}
