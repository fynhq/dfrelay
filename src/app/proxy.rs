use super::*;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StartPlan {
    pub(crate) config: Config,
    pub(crate) nodes: Vec<Node>,
    pub(crate) preferred: Option<String>,
    pub(crate) binary_identity: Option<String>,
}

pub(crate) struct ProxyRuntime {
    pub(crate) status: ProxyStatus,
    pub(crate) proxy: Option<RunningProxy>,
    pub(crate) started_at: Option<Instant>,
    pub(crate) metrics_task: Option<JoinHandle<()>>,
    pub(crate) start_task: Option<JoinHandle<()>>,
    pub(crate) stop_task: Option<JoinHandle<()>>,
    pub(crate) restart_in_progress: bool,
    pub(crate) restart_after_stop: bool,
    pub(crate) preflight_in_flight: bool,
    pub(crate) preflight_revalidations: u32,
    pub(crate) preflight_force: bool,
    pub(crate) restart_attempts: u32,
    pub(crate) restart_at: Option<Instant>,
    pub(crate) stable_reset_at: Option<Instant>,
    pub(crate) runtime_has_auto: bool,
    pub(crate) proxy_node_tags: Vec<String>,
    pub(crate) proxy_auto_tags: HashSet<String>,
    pub(crate) proxy_node_names: HashMap<String, String>,
    pub(crate) last_running_plan: Option<StartPlan>,
    pub(crate) pending_restart_plan: Option<StartPlan>,
    pub(crate) restart_fallback_plan: Option<StartPlan>,
    pub(crate) epoch: u64,
}

impl Default for ProxyRuntime {
    fn default() -> Self {
        Self {
            status: ProxyStatus::Stopped,
            proxy: None,
            started_at: None,
            metrics_task: None,
            start_task: None,
            stop_task: None,
            restart_in_progress: false,
            restart_after_stop: false,
            preflight_in_flight: false,
            preflight_revalidations: 0,
            preflight_force: false,
            restart_attempts: 0,
            restart_at: None,
            stable_reset_at: None,
            runtime_has_auto: false,
            proxy_node_tags: Vec::new(),
            proxy_auto_tags: HashSet::new(),
            proxy_node_names: HashMap::new(),
            last_running_plan: None,
            pending_restart_plan: None,
            restart_fallback_plan: None,
            epoch: 0,
        }
    }
}
