pub const BASE: &str = env!("CARGO_PKG_VERSION");

pub const DISPLAY: &str = env!("DFRELAY_VERSION_DISPLAY");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_base_or_base_plus_hex_suffix() {
        if DISPLAY == BASE {
            return;
        }
        let suffix = DISPLAY
            .strip_prefix(BASE)
            .and_then(|rest| rest.strip_prefix('+'))
            .expect("display version must be BASE or BASE+hash");
        assert!(!suffix.is_empty(), "{DISPLAY}");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "{DISPLAY}");
    }
}
