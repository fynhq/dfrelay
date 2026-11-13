//! Proxy process lifecycle state.

use super::*;

pub(crate) struct ProxyRuntime {
    pub(crate) status: ProxyStatus,
    pub(crate) proxy: Option<RunningProxy>,
    pub(crate) started_at: Option<Instant>,
    pub(crate) metrics_task: Option<JoinHandle<()>>,
    pub(crate) restart_in_progress: bool,
    pub(crate) restart_after_stop: bool,
    pub(crate) preflight_in_flight: bool,
    pub(crate) restart_attempts: u32,
    pub(crate) restart_at: Option<Instant>,
    pub(crate) stable_reset_at: Option<Instant>,
    pub(crate) runtime_has_auto: bool,
    pub(crate) proxy_node_tags: Vec<String>,
    pub(crate) proxy_node_names: HashMap<String, String>,
}

impl Default for ProxyRuntime {
    fn default() -> Self {
        Self {
            status: ProxyStatus::Stopped,
            proxy: None,
            started_at: None,
            metrics_task: None,
            restart_in_progress: false,
            restart_after_stop: false,
            preflight_in_flight: false,
            restart_attempts: 0,
            restart_at: None,
            stable_reset_at: None,
            runtime_has_auto: false,
            proxy_node_tags: Vec::new(),
            proxy_node_names: HashMap::new(),
        }
    }
}
