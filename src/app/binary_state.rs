//! sing-box binary version, setup and update-check state.

use super::*;

#[derive(Debug, Default)]
pub(crate) struct BinaryState {
    pub(crate) binary_version: Option<String>,
    pub(crate) binary_verified: Option<bool>,
    pub(crate) binary_installed: bool,
    pub(crate) latest_binary_version: Option<String>,
    pub(crate) previous_binary_available: bool,
    pub(crate) previous_binary_version: Option<String>,
    pub(crate) update_check_in_flight: bool,
    pub(crate) last_update_check: Option<Instant>,
    pub(crate) setup: SetupState,
    pub(crate) install_in_flight: bool,
}
