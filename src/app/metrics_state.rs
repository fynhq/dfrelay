//! Node, metrics, IP and viewer-observation state.

use super::*;

pub(crate) struct MetricsState {
    pub(crate) nodes: Vec<Node>,
    pub(crate) auto_tags_cache: Option<(u64, Vec<String>)>,
    pub(crate) metrics: MetricsSnapshot,
    pub(crate) node_delays: HashMap<String, u64>,
    pub(crate) delay_testing: HashSet<String>,
    pub(crate) up_speed: f64,
    pub(crate) down_speed: f64,
    pub(crate) last_totals: Option<(u64, u64, Instant)>,
    pub(crate) local_ips: Vec<String>,
    pub(crate) proxy_ips: Vec<String>,
    pub(crate) last_local_ips: Instant,
    pub(crate) last_proxy_ips: Instant,
    pub(crate) proxy_ips_in_flight: bool,
    pub(crate) proxy_ip_backoff_secs: u64,
    pub(crate) proxy_ip_failures: u32,
    pub(crate) delay_sweep_running: bool,
    pub(crate) last_delay_sweep: Option<Instant>,
    pub(crate) idle_since: Option<Instant>,
    pub(crate) viewer_active: Arc<AtomicBool>,
    pub(crate) metrics_wake: Arc<Notify>,
    pub(crate) proxies_refresh: Arc<AtomicBool>,
}

impl Default for MetricsState {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            auto_tags_cache: None,
            metrics: MetricsSnapshot::default(),
            node_delays: HashMap::new(),
            delay_testing: HashSet::new(),
            up_speed: 0.0,
            down_speed: 0.0,
            last_totals: None,
            local_ips: Vec::new(),
            proxy_ips: Vec::new(),
            last_local_ips: Instant::now(),
            last_proxy_ips: Instant::now(),
            proxy_ips_in_flight: false,
            proxy_ip_backoff_secs: 5,
            proxy_ip_failures: 0,
            delay_sweep_running: false,
            last_delay_sweep: None,
            idle_since: None,
            viewer_active: Arc::new(AtomicBool::new(false)),
            metrics_wake: Arc::new(Notify::new()),
            proxies_refresh: Arc::new(AtomicBool::new(false)),
        }
    }
}
