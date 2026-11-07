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

/// Shadowsocks methods supported by the bundled sing-box baseline. Legacy
/// stream ciphers are intentionally rejected here: sing-box refuses them, and
/// one bad node would otherwise fail the whole generated configuration.
pub(super) fn ensure_shadowsocks_method(method: &str) -> Result<(), String> {
    match method {
        "none"
        | "aes-128-gcm"
        | "aes-192-gcm"
        | "aes-256-gcm"
        | "chacha20-ietf-poly1305"
        | "xchacha20-ietf-poly1305"
        | "2022-blake3-aes-128-gcm"
        | "2022-blake3-aes-256-gcm"
        | "2022-blake3-chacha20-poly1305" => Ok(()),
        other => Err(format!("unsupported shadowsocks cipher: {other}")),
    }
}

/// VMess security values accepted by sing-box.
pub(super) fn ensure_vmess_security(security: &str) -> Result<(), String> {
    match security {
        "auto" | "none" | "zero" | "aes-128-gcm" | "chacha20-poly1305" => Ok(()),
        other => Err(format!("unsupported vmess security: {other}")),
    }
}

/// VLESS flow values accepted by sing-box.
pub(super) fn ensure_vless_flow(flow: &str) -> Result<(), String> {
    match flow {
        "xtls-rprx-vision" => Ok(()),
        other => Err(format!("unsupported vless flow: {other}")),
    }
}

/// Hysteria2 obfuscation types accepted by sing-box.
pub(super) fn ensure_hysteria2_obfs(obfs: &str) -> Result<(), String> {
    match obfs {
        "salamander" => Ok(()),
        other => Err(format!("unsupported hysteria2 obfs: {other}")),
    }
}

/// TUIC congestion control algorithms accepted by sing-box.
pub(super) fn ensure_tuic_congestion(congestion: &str) -> Result<(), String> {
    match congestion {
        "cubic" | "new_reno" | "bbr" => Ok(()),
        other => Err(format!("unsupported TUIC congestion control: {other}")),
    }
}

/// TUIC UDP relay modes accepted by sing-box.
pub(super) fn ensure_tuic_udp_relay_mode(mode: &str) -> Result<(), String> {
    match mode {
        "native" | "quic" => Ok(()),
        other => Err(format!("unsupported TUIC UDP relay mode: {other}")),
    }
}

fn is_supported_fingerprint(fingerprint: &str) -> bool {
    matches!(
        fingerprint,
        "chrome"
            | "firefox"
            | "safari"
            | "ios"
            | "android"
            | "edge"
            | "360"
            | "qq"
            | "random"
            | "randomized"
    )
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
    if let Some(fingerprint) = fingerprint.filter(|value| is_supported_fingerprint(value)) {
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
