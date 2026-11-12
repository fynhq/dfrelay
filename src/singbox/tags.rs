//! Fixed outbound tags used by the generated sing-box configuration.
//!
//! The selector, urltest group and direct outbound names are part of the
//! Clash API contract; keeping them in one place prevents the generator and
//! the API client from drifting apart.

pub const SELECTOR: &str = "proxy";
pub const AUTO: &str = "auto";
pub const DIRECT: &str = "direct";
