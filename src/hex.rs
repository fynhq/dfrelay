//! Lowercase hexadecimal encoding for identifiers and digests.

use std::fmt::Write as _;

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_lowercase_hex() {
        assert_eq!(encode(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
        assert_eq!(encode(&[]), "");
    }
}
