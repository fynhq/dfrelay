use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::time::Duration;

use reqwest::Proxy;

#[derive(Debug, thiserror::Error)]
pub enum NetInfoError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("proxy response is not an IP address")]
    InvalidIp,
    #[error("invalid proxy URL: {0}")]
    InvalidProxy(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Linux,
    Macos,
    #[allow(dead_code)]
    Other,
}

impl Platform {
    fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::Macos
        }
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Self::Other
        }
    }
}

#[derive(Debug, Clone)]
struct InterfaceAddress {
    interface: String,
    ip: IpAddr,
    is_p2p: bool,
}

pub fn local_addresses(include_link_local: bool) -> Vec<String> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let entries = interfaces
        .into_iter()
        .map(|interface| InterfaceAddress {
            interface: interface.name,
            ip: interface.addr.ip(),
            is_p2p: interface.is_p2p,
        })
        .collect();
    select_local_addresses(entries, include_link_local, Platform::current())
}

fn select_local_addresses(
    entries: Vec<InterfaceAddress>,
    include_link_local: bool,
    platform: Platform,
) -> Vec<String> {
    let mut ipv4: BTreeMap<String, BTreeSet<Ipv4Addr>> = BTreeMap::new();
    let mut ipv6: BTreeMap<String, Vec<Ipv6Addr>> = BTreeMap::new();
    for entry in entries {
        if entry.is_p2p || is_virtual_interface(&entry.interface, platform) {
            continue;
        }
        match entry.ip {
            IpAddr::V4(ip) => {
                if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast()
                {
                    continue;
                }
                if ip.is_link_local() && !include_link_local {
                    continue;
                }
                ipv4.entry(entry.interface).or_default().insert(ip);
            }
            IpAddr::V6(ip) => {
                if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
                    continue;
                }
                if ip.is_unicast_link_local() && !include_link_local {
                    continue;
                }
                ipv6.entry(entry.interface).or_default().push(ip);
            }
        }
    }

    let interfaces: BTreeSet<String> = ipv4.keys().chain(ipv6.keys()).cloned().collect();
    let mut result = Vec::new();
    for interface in interfaces {
        if let Some(addresses) = ipv4.get(&interface) {
            for ip in addresses {
                result.push(format!("{ip} ({interface})"));
            }
        }
        if let Some(addresses) = ipv6.get_mut(&interface) {
            addresses.sort_by_key(|ip| (ipv6_rank(*ip), *ip));
            if let Some(ip) = addresses.first() {
                result.push(format!("{ip} ({interface})"));
            }
        }
    }
    result
}

fn ipv6_rank(ip: Ipv6Addr) -> u8 {
    if ip.is_unicast_link_local() {
        2
    } else if ip.is_unique_local() {
        1
    } else {
        0
    }
}

fn is_virtual_interface(name: &str, platform: Platform) -> bool {
    let name = name.to_ascii_lowercase();
    if name == "lo" || name == "lo0" || name.starts_with("lo0.") {
        return true;
    }
    if platform == Platform::Macos && name.starts_with("bridge") {
        return true;
    }
    // `br-*` is Docker; `br0` on Linux may be the primary LAN bridge and is kept.
    const PREFIXES: &[&str] = &[
        "utun",
        "awdl",
        "llw",
        "anpi",
        "vmenet",
        "vmnet",
        "vboxnet",
        "docker",
        "veth",
        "virbr",
        "br-",
        "cni",
        "flannel",
        "podman",
        "lxc",
        "lxd",
        "incus",
        "kube",
        "dummy",
        "ifb",
        "nlmon",
        "gretap",
        "erspan",
        "tun",
        "tap",
        "wg",
        "tailscale",
        "zt",
        "ipsec",
        "gif",
        "stf",
        "ppp",
    ];
    if PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
        return true;
    }
    name.starts_with("ap") && name.chars().nth(2).is_some_and(|ch| ch.is_ascii_digit())
}

pub fn local_proxy_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub fn local_proxy_url_with_auth(port: u16, username: &str, password: &str) -> String {
    let mut url = url::Url::parse(&local_proxy_url(port)).expect("local proxy URL is valid");
    if url.set_username(username).is_ok() {
        let _ = url.set_password(Some(password));
    }
    url.to_string()
}

pub async fn fetch_proxy_ip(url: &str, proxy_url: &str) -> Result<String, NetInfoError> {
    // The echo endpoints are HTTPS, so the proxy must intercept all schemes.
    let proxy = Proxy::all(proxy_url).map_err(|err| NetInfoError::InvalidProxy(err.to_string()))?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(8))
        .build()?;
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(NetInfoError::InvalidProxy(format!(
            "HTTP {}",
            response.status()
        )));
    }
    let text = response.text().await?;
    let text = text.trim();
    IpAddr::from_str(text)
        .map(|ip| ip.to_string())
        .map_err(|_| NetInfoError::InvalidIp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_proxy_url_is_loopback_http() {
        assert_eq!(local_proxy_url(12000), "http://127.0.0.1:12000");
        assert_eq!(
            local_proxy_url_with_auth(12000, "us er", "p@ss"),
            "http://us%20er:p%40ss@127.0.0.1:12000/"
        );
    }

    #[test]
    fn local_addresses_do_not_include_loopback() {
        for address in local_addresses(true) {
            assert!(!address.starts_with("127."));
            assert!(!address.starts_with("::1"));
        }
    }

    fn address(interface: &str, ip: &str, is_p2p: bool) -> InterfaceAddress {
        InterfaceAddress {
            interface: interface.to_string(),
            ip: ip.parse().unwrap(),
            is_p2p,
        }
    }

    #[test]
    fn filters_virtual_interfaces_and_collapses_ipv6() {
        let entries = vec![
            address("en0", "192.0.2.10", false),
            address("en0", "2001:db8::10", false),
            address("en0", "2001:db8::11", false),
            address("bridge100", "198.51.100.3", false),
            address("bridge100", "fd00::10", false),
            address("utun0", "203.0.113.2", true),
            address("docker0", "198.51.100.4", false),
            address("lo0", "198.51.100.5", false),
        ];
        assert_eq!(
            select_local_addresses(entries, false, Platform::Macos),
            vec![
                "192.0.2.10 (en0)".to_string(),
                "2001:db8::10 (en0)".to_string(),
            ]
        );
    }

    #[test]
    fn keeps_linux_bridge_and_keeps_ipv4_aliases() {
        let entries = vec![
            address("bridge0", "198.51.100.1", false),
            address("bridge0", "198.51.100.2", false),
        ];
        assert_eq!(
            select_local_addresses(entries.clone(), false, Platform::Linux),
            vec![
                "198.51.100.1 (bridge0)".to_string(),
                "198.51.100.2 (bridge0)".to_string(),
            ]
        );
        assert!(select_local_addresses(entries, false, Platform::Macos).is_empty());
    }

    #[test]
    fn prefers_global_ipv6_over_ula_and_hides_link_local_by_default() {
        let entries = vec![
            address("eth0", "fd00::2", false),
            address("eth0", "2001:db8::2", false),
            address("eth0", "fe80::2", false),
        ];
        assert_eq!(
            select_local_addresses(entries, false, Platform::Linux),
            vec!["2001:db8::2 (eth0)".to_string()]
        );
        let link_local = vec![address("eth0", "fe80::2", false)];
        assert!(select_local_addresses(link_local.clone(), false, Platform::Linux).is_empty());
        assert_eq!(
            select_local_addresses(link_local, true, Platform::Linux),
            vec!["fe80::2 (eth0)".to_string()]
        );
    }

    #[test]
    fn drops_point_to_point_and_virtual_names() {
        let entries = vec![
            address("en7", "203.0.113.10", true),
            address("tun0", "203.0.113.11", false),
            address("tailscale0", "203.0.113.12", false),
            address("lo", "203.0.113.13", false),
            address("en0", "198.51.100.20", false),
        ];
        assert_eq!(
            select_local_addresses(entries, false, Platform::Linux),
            vec!["198.51.100.20 (en0)".to_string()]
        );
    }
}
