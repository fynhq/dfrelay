//! Shared outbound JSON construction used by both subscription parsers.
//!
//! Clash and URI inputs reach this module with plain values, so the emitted
//! `tls` object cannot drift between the two code paths.

use serde_json::{json, Map, Value};

/// Transports DFRelay can translate into a sing-box outbound.
pub(super) fn ensure_supported_network(network: &str) -> Result<(), String> {
    match network.to_ascii_lowercase().as_str() {
        "tcp" | "ws" | "grpc" | "h2" | "http" => Ok(()),
        other => Err(format!("unsupported transport: {other}")),
    }
}

/// Build a sing-box `tls` object.
///
/// `insecure` is `Some(false)` for URI inputs (which always emit the field) and
/// `None` for Clash inputs unless `skip-cert-verify` is set.
#[allow(clippy::too_many_arguments)]
pub(super) fn tls_value(
    enabled: bool,
    server_name: Option<&str>,
    insecure: Option<bool>,
    alpn: &[String],
    fingerprint: Option<&str>,
    reality: Option<(&str, &str)>,
) -> Value {
    let mut tls = Map::new();
    tls.insert("enabled".into(), json!(enabled));
    if let Some(server_name) = server_name {
        tls.insert("server_name".into(), json!(server_name));
    }
    if let Some(insecure) = insecure {
        tls.insert("insecure".into(), json!(insecure));
    }
    if !alpn.is_empty() {
        tls.insert("alpn".into(), json!(alpn));
    }
    if let Some(fingerprint) = fingerprint {
        tls.insert(
            "utls".into(),
            json!({ "enabled": true, "fingerprint": fingerprint }),
        );
    }
    if let Some((public_key, short_id)) = reality {
        tls.insert(
            "reality".into(),
            json!({
                "enabled": true,
                "public_key": public_key,
                "short_id": short_id
            }),
        );
    }
    Value::Object(tls)
}
