//! Regression corpus for untrusted subscription bodies.
//!
//! Outcome-agnostic on purpose: remote input only has to avoid panics and stay
//! within the per-source caps. A well-formed payload may legitimately be
//! rejected by detection, so the tests never unwrap a success.

use super::detect::detect_and_parse;
use super::parse_clash::parse_clash;
use super::parse_uri::{parse_uri, parse_uri_list};
use super::{MAX_NODES_PER_SOURCE, MAX_UNSUPPORTED_PER_SOURCE};

const MALFORMED: &[&str] = &[
    "",
    " ",
    "\0\u{1}\u{2}",
    "ss://",
    "ss://@@@@",
    "ss://YWVzLTEyOC1nY206@example.com:443#a",
    "ss://YWVzLTEyOC1nY206cGFzcw@example.com:notaport#a",
    "ss://plugin=obfs-local;obfs=http@example.com:443#a",
    "vmess://not-base64",
    "vmess://eyJhZGQiOiIiLCJwb3J0Ijo5OTk5OX0=",
    "vless://@:443",
    "vless://00000000-0000-0000-0000-000000000000@example.com:0#a",
    "trojan://",
    "trojan://pass@example.com:0#t",
    "hysteria2://pass@example.com:443?obfs=unknown#h",
    "tuic://@",
    "http://[::1",
    "socks5://",
    "wireguard://abc",
    "proxies: []\n",
    "proxies:\n  - null\n",
    "proxies:\n  - name: a\n    type: ss\n    server: example.com\n    port: abc\n",
    "proxies:\n  - &a {name: a, type: ss, server: example.com, port: 1}\n  - *a\n",
    "Port: 7890\nProxy: ss,example.com,443,encrypt,password\n",
    "{\"outbounds\": [",
    "<!DOCTYPE html><html><body>login</body></html>",
    "\u{feff}",
];

const VALID_URIS: &[&str] = &[
    "ss://YWVzLTEyOC1nY206c2VjcmV0@example.com:443#node",
    "trojan://secret@example.com:443?sni=example.com#node",
    "vless://00000000-0000-0000-0000-000000000000@example.com:443?security=tls#node",
    "hysteria2://secret@example.com:443#node",
    "https://user:pass@example.com#node",
];

fn assert_bounded(nodes: usize, unsupported: usize) {
    assert!(nodes <= MAX_NODES_PER_SOURCE);
    assert!(unsupported <= MAX_UNSUPPORTED_PER_SOURCE);
}

#[test]
fn malformed_bodies_never_panic_and_stay_bounded() {
    for body in MALFORMED {
        if let Ok(outcome) = detect_and_parse(body, "test") {
            assert_bounded(outcome.nodes.len(), outcome.unsupported.len());
        }
        if let Ok((nodes, unsupported)) = parse_uri_list(body, "test") {
            assert_bounded(nodes.len(), unsupported.len());
        }
        let _ = parse_clash(body, "test");
        let _ = parse_uri(body, "test");
    }
}

#[test]
fn truncated_uris_never_panic() {
    for uri in VALID_URIS {
        for (idx, _) in uri.char_indices() {
            let prefix = &uri[..idx];
            let _ = parse_uri(prefix, "test");
            if let Ok((nodes, unsupported)) = parse_uri_list(prefix, "test") {
                assert_bounded(nodes.len(), unsupported.len());
            }
        }
    }
}

#[test]
fn deterministic_random_corpus_never_panics() {
    const ALPHABET: &[u8] =
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789:/@?#&=.-_%[]{}'\"\\ \n\t";
    let mut state = 0x5eed_c0de_u64;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state
    };

    for _ in 0..200 {
        let len = (next() % 2048) as usize;
        let mut body = String::with_capacity(len);
        for _ in 0..len {
            body.push(ALPHABET[(next() as usize) % ALPHABET.len()] as char);
        }
        if let Ok(outcome) = detect_and_parse(&body, "test") {
            assert_bounded(outcome.nodes.len(), outcome.unsupported.len());
        }
        if let Ok((nodes, unsupported)) = parse_uri_list(&body, "test") {
            assert_bounded(nodes.len(), unsupported.len());
        }
        let _ = parse_clash(&body, "test");
    }

    let long = "a".repeat(64 * 1024);
    let _ = detect_and_parse(&long, "test");
}
