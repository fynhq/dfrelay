use serde_json::{json, Map, Value};

pub(super) fn ensure_supported_network(network: &str) -> Result<(), String> {
    match network.to_ascii_lowercase().as_str() {
        "tcp" | "ws" | "grpc" | "h2" | "http" => Ok(()),
        other => Err(format!("unsupported transport: {other}")),
    }
}

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

pub(super) fn ensure_vmess_security(security: &str) -> Result<(), String> {
    match security {
        "auto" | "none" | "zero" | "aes-128-gcm" | "chacha20-poly1305" => Ok(()),
        other => Err(format!("unsupported vmess security: {other}")),
    }
}

pub(super) fn ensure_vless_flow(flow: &str) -> Result<(), String> {
    match flow {
        "xtls-rprx-vision" => Ok(()),
        other => Err(format!("unsupported vless flow: {other}")),
    }
}

pub(super) fn ensure_hysteria2_obfs(obfs: &str) -> Result<(), String> {
    match obfs {
        "salamander" | "gecko" => Ok(()),
        other => Err(format!("unsupported hysteria2 obfs: {other}")),
    }
}

pub(super) fn ensure_uuid(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        });
    if valid {
        Ok(())
    } else {
        Err("invalid uuid format".to_string())
    }
}

pub(super) fn ensure_reality_public_key(value: &str) -> Result<(), String> {
    use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
    use base64::Engine;
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .or_else(|_| URL_SAFE.decode(value));
    if decoded.is_ok_and(|bytes| bytes.len() == 32) {
        Ok(())
    } else {
        Err("invalid reality public key".to_string())
    }
}

pub(super) fn ensure_reality_short_id(value: &str) -> Result<(), String> {
    if value.len() <= 16
        && value.len().is_multiple_of(2)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err("invalid reality short id".to_string())
    }
}

pub(super) fn parse_port_ranges(value: &str) -> Result<Vec<String>, String> {
    const MAX_RANGES: usize = 64;
    let mut ranges = Vec::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        let (lo, hi) = match entry.split_once(['-', ':']) {
            Some((lo, hi)) => (lo, hi),
            None => (entry, entry),
        };
        let lo: u16 = lo.parse().map_err(|_| "invalid port list".to_string())?;
        let hi: u16 = hi.parse().map_err(|_| "invalid port list".to_string())?;
        if lo == 0 || hi == 0 || lo > hi {
            return Err("invalid port list".to_string());
        }
        if ranges.len() >= MAX_RANGES {
            return Err("port list too long".to_string());
        }
        // sing-box requires lo:hi range entries; a bare port is rejected by check.
        ranges.push(format!("{lo}:{hi}"));
    }
    if ranges.is_empty() {
        return Err("invalid port list".to_string());
    }
    Ok(ranges)
}

pub(super) fn ensure_tuic_congestion(congestion: &str) -> Result<(), String> {
    match congestion {
        "cubic" | "new_reno" | "bbr" => Ok(()),
        other => Err(format!("unsupported TUIC congestion control: {other}")),
    }
}

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

// sing-box removed these legacy chrome fingerprints in 1.10 and falls back to chrome.
fn is_legacy_chrome_fingerprint(fingerprint: &str) -> bool {
    matches!(
        fingerprint,
        "chrome_psk"
            | "chrome_psk_shuffle"
            | "chrome_padding_psk_shuffle"
            | "chrome_pq"
            | "chrome_pq_psk"
    )
}

fn resolve_reality_fingerprint(fingerprint: Option<&str>) -> Result<&str, String> {
    match fingerprint {
        None | Some("") => Ok("chrome"),
        Some(value) if is_supported_fingerprint(value) => Ok(value),
        Some(value) if is_legacy_chrome_fingerprint(value) => Ok("chrome"),
        Some(value) => Err(format!("unsupported uTLS fingerprint for reality: {value}")),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn tls_value(
    enabled: bool,
    server_name: Option<&str>,
    insecure: Option<bool>,
    alpn: &[String],
    fingerprint: Option<&str>,
    reality: Option<(&str, &str)>,
) -> Result<Value, String> {
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
    // REALITY requires uTLS; without a usable fingerprint sing-box rejects the whole config.
    let fingerprint = match reality {
        Some(_) => Some(resolve_reality_fingerprint(fingerprint)?),
        None => fingerprint.filter(|value| is_supported_fingerprint(value)),
    };
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
    Ok(Value::Object(tls))
}

pub(super) fn tls_string_enabled(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "none" | "false" | "0"
    )
}

pub(super) fn split_alpn_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}
