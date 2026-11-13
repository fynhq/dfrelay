use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::net::UnixListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};

use crate::config::{Config, Paths, SubscriptionConfig};
use crate::i18n::Text;
use crate::ipc::{self, ServerMsg};
use crate::logging::{LogLevel, LogSource, LogTimestamp, Logger};
use crate::netinfo;
use crate::service::ServiceIdentity;
use crate::singbox::binary::{is_newer_version, BinaryManager, BinaryMetadata};
use crate::singbox::clash::{ClashClient, ClashError, MetricsSnapshot, ProxySelection};
use crate::singbox::supervisor::{self, RunningProxy};
use crate::singbox::tags;
use crate::state::{
    settings_items, EditForm, KeywordForm, LineInput, Overlay, Page, ProxyStatus, SettingsForm,
    SetupState, SubscriptionForm,
};
use crate::subscription::model::Node;
use crate::subscription::{self, merge_nodes, FetchOptions, SubscriptionRuntime};
use crate::text::{sanitize_display, sanitize_owned};
use crate::view::{BinaryStatus, ConfigView, DaemonStatus, View};

mod binary_state;
mod health;
mod keys;
mod metrics_state;
mod proxy;
mod quality;
mod subscriptions;
mod ui_state;
mod updates;

use binary_state::BinaryState;
use health::HealthState;
use metrics_state::MetricsState;
use proxy::ProxyRuntime;
use quality::QualityBook;
use subscriptions::SubscriptionState;
use ui_state::{NodeSort, UiState};

const MAX_RESTART_ATTEMPTS: u32 = 5;
const RESTART_DELAYS: [u64; MAX_RESTART_ATTEMPTS as usize] = [1, 2, 5, 10, 30];
const DELAY_TEST_CONCURRENCY: usize = 4;
const DELAY_SWEEP_COOLDOWN: Duration = Duration::from_secs(300);
const HEALTH_PROBE_INTERVAL: Duration = Duration::from_secs(10);
const HEALTH_ACTIVE_WINDOW: Duration = Duration::from_secs(120);
const HEALTH_IDLE_PROBE_INTERVAL: Duration = Duration::from_secs(120);
const HEALTH_FAST_PROBE_GAP: Duration = Duration::from_secs(1);
const HEALTH_PROBE_STALE: Duration = Duration::from_secs(5);
const HEALTH_FAILURE_THRESHOLD: u32 = 3;
const HEALTH_RECOVERY_COOLDOWN: Duration = Duration::from_secs(120);
const HEALTH_NO_NODE_BACKOFF_SECS: [u64; 3] = [60, 120, 300];
const HEALTH_REFRESH_COOLDOWN: Duration = Duration::from_secs(600);
const FALLBACK_TEST_URL: &str = "https://cp.cloudflare.com/generate_204";
const AUTO_SWITCH_BUDGET: usize = 6;
const AUTO_SWITCH_WINDOW: Duration = Duration::from_secs(60 * 60);
const AUTO_SWITCH_PAUSE: Duration = Duration::from_secs(10 * 60);
const SELECTION_STORM_WINDOW: Duration = Duration::from_secs(60 * 60);
const SELECTION_STORM_THRESHOLD: usize = 30;
const SELECTION_STORM_WARN_COOLDOWN: Duration = Duration::from_secs(60 * 60);
const SWITCH_REASON_TTL: Duration = Duration::from_secs(15);
const REFRESH_IDLE_GRACE: Duration = Duration::from_secs(10);
const REFRESH_IDLE_WAIT_LOG: Duration = Duration::from_secs(5 * 60);
const PROXY_IP_STARTUP_GRACE: Duration = Duration::from_secs(30);
const SUBSCRIPTION_STARTUP_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const SUBSCRIPTION_REFRESH_MIN_BACKOFF: Duration = Duration::from_secs(5 * 60);
const SUBSCRIPTION_REFRESH_MAX_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);
const IDLE_METRICS_INTERVAL: Duration = Duration::from_secs(3);
const PROXIES_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
pub const MAX_LIST_ROWS: usize = 200;
pub const TOAST_TTL: Duration = Duration::from_secs(3);
pub const VIEW_QUEUE_LIMIT: usize = 2;
pub const MAX_CLIENTS: usize = 16;
/// Bound on the connection IDs kept for new-traffic detection. The Clash API
/// can return an unbounded list; hashing keeps the guard cheap while still
/// noticing new connections on a busy proxy.
const MAX_HEALTH_CONNECTION_IDS: usize = 8192;

fn connection_id_hash(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

/// Process-lifetime counters for diagnosing selection churn without adding
/// user-facing configuration. The rolling deque is bounded by time, not by a
/// hard capacity; selection changes are logged at most once per window.
#[derive(Debug, Default)]
struct RuntimeStats {
    selection_change_times: VecDeque<Instant>,
    selection_changes_total: u64,
    last_storm_warn: Option<Instant>,
    health_probes_total: u64,
    health_probe_failures_total: u64,
    recovery_attempts: u64,
    recovery_selected: u64,
    recovery_aborted: u64,
    recovery_no_healthy: u64,
}

fn edit_line_input(input: &mut LineInput, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Backspace => input.backspace(),
        KeyCode::Delete => input.delete(),
        KeyCode::Left => input.left(),
        KeyCode::Right => input.right(),
        KeyCode::Home => input.home(),
        KeyCode::End => input.end(),
        KeyCode::Char(ch)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            input.insert(ch);
        }
        _ => return false,
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingSelection {
    Auto,
    Node(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthOutcome {
    Healthy(u64),
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecoveryTarget {
    Auto,
    Node(String),
}

fn health_outcome(result: &Result<u64, ClashError>) -> HealthOutcome {
    match result {
        Ok(delay) if *delay > 0 => HealthOutcome::Healthy(*delay),
        Ok(_) => HealthOutcome::Failed,
        Err(ClashError::Http(status)) if *status == 408 || (500..=599).contains(status) => {
            HealthOutcome::Failed
        }
        Err(_) => HealthOutcome::Unavailable,
    }
}

fn choose_recovery_target(
    selected: Option<&str>,
    healthy: &[(String, u64)],
    quality: &QualityBook,
) -> Option<RecoveryTarget> {
    if healthy.is_empty() {
        return None;
    }
    if selected == Some(tags::AUTO) {
        return Some(RecoveryTarget::Auto);
    }
    healthy
        .iter()
        .min_by(|(left, left_delay), (right, right_delay)| {
            quality
                .score(left, Some(*left_delay))
                .total_cmp(&quality.score(right, Some(*right_delay)))
                .then_with(|| left.cmp(right))
        })
        .map(|(tag, _)| RecoveryTarget::Node(tag.clone()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundTask {
    SubscriptionRefresh,
    ProxyStart,
    ProxyStop,
    ProxyIps,
    DelaySweep,
    HealthProbe,
    Recovery,
    BinaryDownload,
    BinaryInstall,
    BinaryRollback,
    BinaryUpdateCheck,
    Preflight,
    Metrics,
    DelayTest,
    Selection,
    Ipc,
    Other,
}

pub enum AppEvent {
    Key(KeyEvent),
    BackgroundPanic {
        task: BackgroundTask,
        message: String,
    },
    PreflightDone(Result<(), String>),
    Subscriptions {
        seq: u64,
        runtimes: Vec<SubscriptionRuntime>,
    },
    SubscriptionUpdated {
        seq: u64,
        runtime: SubscriptionRuntime,
    },
    DownloadDone(Result<BinaryMetadata, String>),
    InstallDone(Result<BinaryMetadata, String>),
    ProxyStarted(Result<RunningProxy, String>),
    ProxyStopped,
    Metrics(MetricsSnapshot),
    ProxyIps {
        ips: Vec<String>,
        errors: Vec<String>,
    },
    HealthProbe {
        generation: u64,
        tag: String,
        outcome: HealthOutcome,
    },
    RecoveryProbe {
        generation: u64,
        trigger: String,
        healthy: Vec<(String, u64)>,
    },
    NodeDelay {
        tag: String,
        delay: Option<u64>,
    },
    NodeSelected(String),
    SelectionApplied,
    NodeDelaySweepDone,
    DashboardViewport(usize),
    HelpViewport(usize),
    LogDetailViewport(usize),
    UpdateCheckDone(Result<String, String>),
    RollbackDone(Result<BinaryMetadata, String>),
    Info(String),
    Error(String),
    ClientAttached {
        id: u64,
        viewer: bool,
        tx: mpsc::Sender<ServerMsg>,
        accept: tokio::sync::oneshot::Sender<bool>,
    },
    ClientDetached(u64),
    StartProxyRequest {
        client_id: u64,
    },
    StatusRequest {
        client_id: u64,
    },
    ShutdownRequest {
        client_id: u64,
    },
    DaemonSignal,
}

struct ClientHandle {
    viewer: bool,
    tx: mpsc::Sender<ServerMsg>,
}

pub struct App {
    pub config: Config,
    pub paths: Paths,
    pub logger: Logger,
    pub(crate) ui_state: UiState,
    pub(crate) proxy_runtime: ProxyRuntime,
    pub(crate) metrics_state: MetricsState,
    pub(crate) binary_state: BinaryState,
    subscription_state: SubscriptionState,
    managed: bool,
    service: Option<ServiceIdentity>,
    pub should_quit: bool,
    health: HealthState,
    stats: RuntimeStats,
    pub preferred_node: Option<String>,
    selection_generation: Arc<AtomicU64>,
    pending_selection: Option<PendingSelection>,
    pending_start: Option<u64>,
    binary_manager: BinaryManager,
    events_tx: UnboundedSender<AppEvent>,
    events_rx: UnboundedReceiver<AppEvent>,
    clients: HashMap<u64, ClientHandle>,
}

impl App {
    fn edit_form(&self) -> Option<&EditForm> {
        match &self.ui_state.overlay {
            Some(Overlay::Edit(form)) => Some(form),
            _ => None,
        }
    }

    fn edit_form_mut(&mut self) -> Option<&mut EditForm> {
        match &mut self.ui_state.overlay {
            Some(Overlay::Edit(form)) => Some(form),
            _ => None,
        }
    }

    pub(crate) fn subscription_form(&self) -> Option<&SubscriptionForm> {
        match self.edit_form() {
            Some(EditForm::Subscription(form)) => Some(form),
            _ => None,
        }
    }

    fn subscription_form_mut(&mut self) -> Option<&mut SubscriptionForm> {
        match self.edit_form_mut() {
            Some(EditForm::Subscription(form)) => Some(form),
            _ => None,
        }
    }

    pub(crate) fn keyword_form(&self) -> Option<&KeywordForm> {
        match self.edit_form() {
            Some(EditForm::Keyword(form)) => Some(form),
            _ => None,
        }
    }

    fn keyword_form_mut(&mut self) -> Option<&mut KeywordForm> {
        match self.edit_form_mut() {
            Some(EditForm::Keyword(form)) => Some(form),
            _ => None,
        }
    }

    pub(crate) fn settings_form(&self) -> Option<&SettingsForm> {
        match self.edit_form() {
            Some(EditForm::Settings(form)) => Some(form),
            _ => None,
        }
    }

    fn settings_form_mut(&mut self) -> Option<&mut SettingsForm> {
        match self.edit_form_mut() {
            Some(EditForm::Settings(form)) => Some(form),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn log_detail(&self) -> Option<&crate::logging::LogLine> {
        match &self.ui_state.overlay {
            Some(Overlay::LogDetail { line, .. }) => Some(line),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn log_detail_scroll(&self) -> usize {
        match &self.ui_state.overlay {
            Some(Overlay::LogDetail { scroll, .. }) => *scroll,
            _ => 0,
        }
    }

    fn close_overlay(&mut self) {
        if matches!(self.ui_state.overlay, Some(Overlay::Setup { .. })) {
            self.binary_state.setup.pending_install = None;
            self.binary_state.setup.message = None;
            self.ui_state.rollback_confirm = false;
        }
        self.ui_state.overlay = None;
        self.ui_state.log_detail_max_scroll = 0;
    }

    fn set_edit_error(&mut self, message: String) {
        if let Some(form) = self.edit_form_mut() {
            form.set_error(Some(message));
        }
    }

    fn save_config_changes(&mut self, config: Config) -> Result<(), String> {
        config
            .save(&self.paths.config_file)
            .map_err(|err| err.to_string())?;
        self.config = config;
        Ok(())
    }

    pub async fn new(config: Config, paths: Paths, logger: Logger) -> Self {
        let binary_manager = BinaryManager::new(paths.clone());
        let mut setup = SetupState::default();
        let locator = binary_manager.locate();
        let binary_installed = locator.is_some();
        let mut binary_version = None;
        let mut binary_verified = None;
        if locator.is_none() {
            setup.candidates = binary_manager.scan_inbox().unwrap_or_default();
        } else if let Some(binary) = locator.as_ref() {
            let metadata = binary_manager.metadata();
            binary_version = binary_manager
                .version(binary)
                .await
                .ok()
                .or_else(|| metadata.as_ref().map(|metadata| metadata.version.clone()));
            binary_verified = metadata.map(|metadata| metadata.verified);
        }
        let overlay = if locator.is_none() {
            Some(Overlay::Setup { selected: 0 })
        } else {
            None
        };
        let mut cached_subscriptions =
            subscription::cache::load(&paths.subscription_cache(), &config.subscriptions);
        for subscription_config in &config.subscriptions {
            if !cached_subscriptions
                .iter()
                .any(|runtime| runtime.config.id == subscription_config.id)
            {
                cached_subscriptions.push(empty_runtime(subscription_config.clone()));
            }
        }
        let cached_nodes = merge_nodes(&cached_subscriptions);
        let startup_cache_fresh = cached_subscription_age(&cached_subscriptions)
            .is_some_and(|age| age < SUBSCRIPTION_STARTUP_MAX_AGE);
        let has_enabled = config.enabled_subscriptions().next().is_some();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let mut app = Self {
            config,
            paths,
            logger,
            ui_state: UiState {
                overlay,
                ..UiState::default()
            },
            proxy_runtime: ProxyRuntime::default(),
            metrics_state: MetricsState {
                nodes: cached_nodes,
                local_ips: netinfo::local_addresses(false),
                ..MetricsState::default()
            },
            subscription_state: SubscriptionState {
                subscriptions: cached_subscriptions,
                last_subscription_refresh: startup_cache_fresh.then(Instant::now),
                subscription_refresh_backoff: SUBSCRIPTION_REFRESH_MIN_BACKOFF,
                ..SubscriptionState::default()
            },
            binary_state: BinaryState {
                binary_version,
                binary_verified,
                binary_installed,
                setup,
                ..BinaryState::default()
            },
            managed: false,
            service: None,
            should_quit: false,
            health: HealthState::default(),
            stats: RuntimeStats::default(),
            preferred_node: None,
            selection_generation: Arc::new(AtomicU64::new(0)),
            pending_selection: None,
            pending_start: None,
            binary_manager,
            events_tx,
            events_rx,
            clients: HashMap::new(),
        };
        if has_enabled && !startup_cache_fresh {
            app.refresh_subscriptions();
        }
        app.refresh_previous_binary_state();
        app.maybe_check_binary_update();
        app
    }

    pub fn text(&self) -> &'static Text {
        self.config.language.text()
    }

    pub fn view(&self) -> View {
        let toast = self
            .ui_state
            .message
            .clone()
            .filter(|_| {
                self.ui_state
                    .message_at
                    .is_none_or(|at| at.elapsed() < TOAST_TTL)
            })
            .map(crate::text::sanitize_owned);
        let show_nodes = self.ui_state.page == Page::Nodes;
        let filtered = if show_nodes {
            self.auto_candidate_tags()
        } else {
            Vec::new()
        };
        let connection_count = self.metrics_state.metrics.connections.len();
        let metrics = MetricsSnapshot {
            upload_total: self.metrics_state.metrics.upload_total,
            download_total: self.metrics_state.metrics.download_total,
            connections: if self.ui_state.page == Page::Connections {
                self.metrics_state.metrics.connections.clone()
            } else {
                Vec::new()
            },
            selection: self.metrics_state.metrics.selection.clone(),
            // Nodes carry their own delay map; avoid shipping a second copy.
            node_delays: HashMap::new(),
        };
        let lan_password_set = !self.config.lan_password.is_empty();
        let config = ConfigView::from_config(&self.config);
        let binary_status = if !self.binary_state.binary_installed {
            BinaryStatus::NotInstalled
        } else if let (Some(latest), Some(current)) = (
            self.binary_state.latest_binary_version.as_deref(),
            self.binary_state.binary_version.as_deref(),
        ) {
            if is_newer_version(latest, current) {
                BinaryStatus::UpgradeAvailable
            } else {
                BinaryStatus::Installed
            }
        } else {
            BinaryStatus::Installed
        };
        let tab_page = match self.ui_state.page {
            Page::Help => self.ui_state.help_return.unwrap_or(Page::Dashboard),
            _ => self.ui_state.page,
        };
        View {
            config,
            lan_password_set,
            page: self.ui_state.page,
            tab_page,
            dashboard_scroll: self.ui_state.dashboard_scroll,
            status: self.proxy_runtime.status.clone(),
            metrics_stale: self.proxy_runtime.status == ProxyStatus::Running
                && self.proxy_runtime.proxy.is_some()
                && self
                    .health
                    .last_metrics_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(5)),
            nodes: if show_nodes {
                self.metrics_state
                    .nodes
                    .iter()
                    .map(|node| crate::view::NodeView {
                        tag: node.tag.clone(),
                        name: sanitize_owned(node.name.clone()),
                        source: sanitize_owned(node.source.clone()),
                        protocol: sanitize_owned(node.protocol.clone()),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            node_count: self.metrics_state.nodes.len(),
            auto_tags: filtered.iter().cloned().collect(),
            auto_candidates: filtered.len(),
            subscriptions: self
                .subscription_state
                .subscriptions
                .iter()
                .map(|runtime| crate::view::SubscriptionView {
                    name: sanitize_owned(runtime.config.name.clone()),
                    url: sanitize_owned(subscription::redact_url(&runtime.config.url)),
                    enabled: runtime.config.enabled,
                    node_count: runtime.nodes.len(),
                    unsupported_count: runtime.unsupported.len(),
                    last_updated: runtime.last_updated.clone(),
                    error: runtime.error.clone().map(sanitize_owned),
                    stale: runtime.stale,
                })
                .collect(),
            selected: self.ui_state.selected,
            preferred_node: self.preferred_node.clone(),
            metrics,
            connection_count,
            node_delays: self.metrics_state.node_delays.clone(),
            delay_testing: self.metrics_state.delay_testing.clone(),
            up_speed: self.metrics_state.up_speed,
            down_speed: self.metrics_state.down_speed,
            local_ips: self.metrics_state.local_ips.clone(),
            proxy_ips: self.metrics_state.proxy_ips.clone(),
            binary_version: self.binary_state.binary_version.clone().map(sanitize_owned),
            binary_verified: self.binary_state.binary_verified,
            binary_status,
            latest_binary_version: self
                .binary_state
                .latest_binary_version
                .clone()
                .map(sanitize_owned),
            previous_binary_available: self.binary_state.previous_binary_available,
            previous_binary_version: self
                .binary_state
                .previous_binary_version
                .clone()
                .map(sanitize_owned),
            setup: {
                let mut setup = self.binary_state.setup.clone();
                setup.message = setup.message.map(sanitize_owned);
                for candidate in &mut setup.candidates {
                    candidate.label = sanitize_owned(std::mem::take(&mut candidate.label));
                }
                setup
            },
            overlay: self.ui_state.overlay.clone(),
            subscription_delete: self.ui_state.subscription_delete.is_some(),
            log_source: self.ui_state.log_source,
            log_filter: self.ui_state.log_filter,
            log_follow: self.ui_state.log_follow,
            log_timestamp: self.ui_state.log_timestamp,
            logs: if self.ui_state.page == Page::Logs {
                self.log_lines()
            } else {
                Vec::new()
            },
            log_file: self.logger.current_file(),
            last_error: self.ui_state.last_error.clone().map(sanitize_owned),
            toast,
            restart_confirm: self.ui_state.restart_confirm,
            rollback_confirm: self.ui_state.rollback_confirm,
            restart_in_progress: self.proxy_runtime.restart_in_progress,
            quitting: self.should_quit,
            help_scroll: self.ui_state.help_scroll,
            binary_path: self.paths.binary().display().to_string(),
            config_path: self.paths.config_file.display().to_string(),
            inbox_path: self.paths.inbox_dir.display().to_string(),
            uptime: self.uptime(),
            listen: self.listen_addresses(),
            current_node: self.current_node(),
            selection_mode_auto: self.selection_mode_auto(),
        }
    }

    pub async fn run_daemon(
        &mut self,
        listener: UnixListener,
        socket_path: PathBuf,
        managed: bool,
        service: Option<ServiceIdentity>,
    ) -> io::Result<()> {
        self.managed = managed;
        self.service = service;
        ipc::spawn_listener(listener, self.events_tx.clone());
        spawn_signal_handler(self.events_tx.clone());
        spawn_auto_start(socket_path.clone(), self.logger.clone());
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        let mut last_publish = Instant::now() - PUBLISH_INTERVAL;
        loop {
            let mut publish = false;
            tokio::select! {
                maybe_event = self.events_rx.recv() => {
                    if let Some(event) = maybe_event {
                        self.handle_event(event).await;
                        // Coalesce bursts (delay sweeps, key repeats) into one snapshot.
                        for _ in 0..32 {
                            let Ok(event) = self.events_rx.try_recv() else {
                                break;
                            };
                            self.handle_event(event).await;
                        }
                        publish = true;
                    }
                }
                _ = ticker.tick() => {
                    publish = self.on_tick().await;
                }
            }
            if publish || last_publish.elapsed() >= PUBLISH_INTERVAL {
                self.publish_view();
                last_publish = Instant::now();
            }
            if self.should_quit {
                break;
            }
        }
        self.broadcast(ServerMsg::Shutdown);
        self.clients.clear();
        if let Some(task) = self.proxy_runtime.metrics_task.take() {
            task.abort();
        }
        if let Some(proxy) = self.proxy_runtime.proxy.take() {
            let logger = self.logger.clone();
            let _ = tokio::time::timeout(Duration::from_secs(6), supervisor::stop(proxy, &logger))
                .await;
        }
        let _ = std::fs::remove_file(&socket_path);
        Ok(())
    }

    async fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key).await,
            AppEvent::BackgroundPanic { task, message } => {
                self.on_background_panic(task, message);
            }
            AppEvent::PreflightDone(result) => {
                self.finish_restart_after_preflight(result);
            }
            AppEvent::Subscriptions { seq, runtimes } => {
                self.subscription_state.refreshing_subscriptions = false;
                let recovery_refresh = self.health.recovery_refresh_pending;
                self.health.recovery_refresh_pending = false;
                let mut accepted: HashMap<String, SubscriptionRuntime> = runtimes
                    .into_iter()
                    .filter(|runtime| {
                        self.subscription_state
                            .subscription_refresh_seq
                            .get(&runtime.config.id)
                            == Some(&seq)
                    })
                    .map(|runtime| (runtime.config.id.clone(), runtime))
                    .collect();
                let mut ordered = Vec::with_capacity(self.config.subscriptions.len());
                for config in &self.config.subscriptions {
                    let mut runtime = accepted
                        .remove(&config.id)
                        .or_else(|| {
                            self.subscription_state
                                .subscriptions
                                .iter()
                                .find(|runtime| runtime.config.id == config.id)
                                .cloned()
                        })
                        .unwrap_or_else(|| empty_runtime(config.clone()));
                    runtime.config = config.clone();
                    ordered.push(runtime);
                }
                self.subscription_state
                    .subscription_refresh_seq
                    .retain(|id, _| {
                        self.config
                            .subscriptions
                            .iter()
                            .any(|config| &config.id == id)
                    });
                let selected_before = self.ui_state.selected;
                let selected_tag = if self.ui_state.page == Page::Nodes {
                    self.metrics_state
                        .nodes
                        .get(selected_before)
                        .map(|node| node.tag.clone())
                } else {
                    None
                };
                let node_count: usize = ordered.iter().map(|item| item.nodes.len()).sum();
                let capped = node_count > subscription::MAX_TOTAL_NODES;
                self.subscription_state.subscriptions = ordered;
                self.refresh_merged_nodes();
                if capped {
                    self.logger.app(
                        LogLevel::Warn,
                        format!(
                            "node cap reached: {node_count} parsed, {} merged (limit {})",
                            self.metrics_state.nodes.len(),
                            subscription::MAX_TOTAL_NODES
                        ),
                    );
                }
                if self
                    .subscription_state
                    .subscriptions
                    .iter()
                    .any(|runtime| runtime.config.enabled && runtime.error.is_some())
                {
                    self.schedule_subscription_retry();
                } else {
                    self.subscription_refresh_succeeded();
                }
                self.ui_state.selected = selected_tag
                    .and_then(|tag| {
                        self.metrics_state
                            .nodes
                            .iter()
                            .position(|node| node.tag == tag)
                    })
                    .unwrap_or_else(|| {
                        selected_before
                            .min(self.max_selection_for(self.ui_state.page).saturating_sub(1))
                    });
                if self.ui_state.page == Page::Logs && self.ui_state.log_follow {
                    self.ui_state.selected = self.max_selection_for(Page::Logs).saturating_sub(1);
                }
                self.finalize_subscription_change();
                self.notify(format!(
                    "{}: {node_count}",
                    self.text().msg_subscriptions_refreshed
                ));
                let unique_count = self.metrics_state.nodes.len();
                let count_detail = if unique_count == node_count {
                    format!("subscriptions refreshed: {node_count} nodes")
                } else {
                    format!("subscriptions refreshed: {node_count} nodes ({unique_count} unique)")
                };
                self.logger.app(LogLevel::Info, count_detail);
                if self
                    .subscription_state
                    .pending_restart_subscription
                    .is_some()
                {
                    self.restart_after_pending_subscription();
                } else {
                    self.note_subscription_apply();
                }
                self.retry_pending_start();
                if recovery_refresh {
                    self.finish_recovery_refresh();
                }
            }
            AppEvent::SubscriptionUpdated { seq, runtime } => {
                let id = runtime.config.id.clone();
                let Some(config) = self
                    .config
                    .subscriptions
                    .iter()
                    .find(|config| config.id == id)
                    .cloned()
                else {
                    return;
                };
                if self.subscription_state.subscription_refresh_seq.get(&id) != Some(&seq) {
                    return;
                }
                let mut runtime = runtime;
                runtime.config = config;
                if let Some(existing) = self
                    .subscription_state
                    .subscriptions
                    .iter_mut()
                    .find(|item| item.config.id == id)
                {
                    *existing = runtime.clone();
                } else {
                    self.subscription_state.subscriptions.push(runtime.clone());
                }
                self.refresh_merged_nodes();
                self.finalize_subscription_change();
                if runtime.error.is_none() {
                    self.notify(format!(
                        "{}: {}",
                        self.text().msg_subscription_saved,
                        runtime.config.name
                    ));
                }
                self.logger.app(
                    LogLevel::Info,
                    format!(
                        "subscription {} refreshed: {} nodes",
                        runtime.config.name,
                        runtime.nodes.len()
                    ),
                );
                if self
                    .subscription_state
                    .pending_restart_subscription
                    .as_deref()
                    == Some(id.as_str())
                {
                    self.restart_after_pending_subscription();
                } else {
                    self.note_subscription_apply();
                }
                self.retry_pending_start();
            }
            AppEvent::DownloadDone(result) => {
                self.binary_state.setup.downloading = false;
                match result {
                    Ok(metadata) => {
                        self.binary_state.binary_version = Some(metadata.version.clone());
                        self.binary_state.binary_verified = Some(metadata.verified);
                        self.binary_state.binary_installed = true;
                        self.binary_state.latest_binary_version = Some(metadata.version.clone());
                        self.binary_state.last_update_check = Some(Instant::now());
                        self.notify(format!(
                            "{} {}",
                            self.text().msg_binary_installed,
                            metadata.version
                        ));
                        self.binary_state.setup.message = None;
                        self.refresh_previous_binary_state();
                        self.switch_page(Page::Dashboard);
                        self.auto_restart_after_change();
                    }
                    Err(err) => {
                        self.binary_state.setup.message =
                            Some(format!("{}: {err}", self.text().setup_failed));
                        self.ui_state.last_error = Some(err);
                    }
                }
            }
            AppEvent::InstallDone(result) => {
                self.binary_state.install_in_flight = false;
                self.binary_state.setup.installing = false;
                match result {
                    Ok(metadata) => {
                        self.binary_state.binary_version = Some(metadata.version.clone());
                        self.binary_state.binary_verified = Some(metadata.verified);
                        self.binary_state.binary_installed = true;
                        self.binary_state.latest_binary_version = None;
                        self.binary_state.last_update_check = None;
                        self.notify(format!(
                            "{} {}",
                            self.text().msg_binary_installed,
                            metadata.version
                        ));
                        self.binary_state.setup.message = None;
                        self.binary_state.setup.pending_install = None;
                        self.refresh_previous_binary_state();
                        self.switch_page(Page::Dashboard);
                        self.maybe_check_binary_update();
                        self.auto_restart_after_change();
                    }
                    Err(err) => {
                        self.binary_state.setup.message =
                            Some(format!("{}: {err}", self.text().setup_failed));
                        self.ui_state.last_error = Some(err);
                    }
                }
            }
            AppEvent::ProxyStarted(result) => match result {
                Ok(proxy) => {
                    let clash = proxy.clash.clone();
                    let ipv6 = proxy.ipv6;
                    self.proxy_runtime.proxy = Some(proxy);
                    self.proxy_runtime.status = ProxyStatus::Running;
                    self.proxy_runtime.restart_in_progress = false;
                    self.proxy_runtime.started_at = Some(Instant::now());
                    self.proxy_runtime.restart_at = None;
                    self.proxy_runtime.stable_reset_at =
                        Some(Instant::now() + Duration::from_secs(60));
                    self.metrics_state.last_totals = None;
                    self.metrics_state.up_speed = 0.0;
                    self.metrics_state.down_speed = 0.0;
                    self.ui_state.last_error = None;
                    self.reset_health_state();
                    self.metrics_state.proxy_ip_backoff_secs = 5;
                    self.metrics_state.proxy_ip_failures = 0;
                    self.start_metrics(clash.port, clash.secret.clone());
                    self.apply_pending_selection(clash.port, clash.secret);
                    self.reconcile_subscription_apply_after_start();
                    self.refresh_proxy_ips();
                    if self.ui_state.page == Page::Nodes {
                        self.maybe_test_node_delays(false);
                    }
                    let message = if self.config.enable_ipv6 && !ipv6 {
                        format!(
                            "{} · {}",
                            self.text().msg_singbox_started,
                            self.text().msg_ipv6_fallback
                        )
                    } else {
                        self.text().msg_singbox_started.to_string()
                    };
                    self.notify(message);
                    self.ack_pending_start(true, "proxy started");
                    self.logger.app(
                        LogLevel::Info,
                        format!("sing-box running (pid {})", self.proxy_pid()),
                    );
                }
                Err(err) => {
                    self.proxy_runtime.status = ProxyStatus::Failed;
                    self.proxy_runtime.restart_in_progress = false;
                    self.proxy_runtime.runtime_has_auto = false;
                    self.proxy_runtime.proxy_node_tags.clear();
                    self.proxy_runtime.proxy_node_names.clear();
                    self.proxy_runtime.stable_reset_at = None;
                    self.ui_state.last_error = Some(err);
                    self.logger.app(LogLevel::Error, "sing-box start failed");
                    let detail = self.ui_state.last_error.clone().unwrap_or_default();
                    self.ack_pending_start(false, format!("proxy start failed: {detail}"));
                    self.schedule_restart();
                }
            },
            AppEvent::ProxyStopped => {
                self.proxy_runtime.status = ProxyStatus::Stopped;
                self.proxy_runtime.runtime_has_auto = false;
                self.proxy_runtime.proxy_node_tags.clear();
                self.proxy_runtime.proxy_node_names.clear();
                self.proxy_runtime.started_at = None;
                self.proxy_runtime.restart_attempts = 0;
                self.proxy_runtime.restart_at = None;
                self.proxy_runtime.stable_reset_at = None;
                self.ui_state.restart_confirm = false;
                self.reset_metrics();
                if self.proxy_runtime.restart_after_stop {
                    self.proxy_runtime.restart_after_stop = false;
                    if self.start_proxy().is_err() {
                        self.proxy_runtime.restart_in_progress = false;
                    }
                } else {
                    self.proxy_runtime.restart_in_progress = false;
                }
            }
            AppEvent::Metrics(mut metrics) => {
                let now = Instant::now();
                if let Some((up, down, at)) = self.metrics_state.last_totals {
                    let seconds = now.duration_since(at).as_secs_f64();
                    if seconds >= 0.2 {
                        self.metrics_state.up_speed =
                            metrics.upload_total.saturating_sub(up) as f64 / seconds;
                        self.metrics_state.down_speed =
                            metrics.download_total.saturating_sub(down) as f64 / seconds;
                    }
                }
                self.metrics_state.last_totals =
                    Some((metrics.upload_total, metrics.download_total, now));
                self.health.last_metrics_at = Some(now);
                // Check and store the same bounded window. Scanning the full
                // list while keeping only the first ids would mark every
                // connection beyond the cap as new on each poll and keep fast
                // health probes permanently armed.
                let connection_ids = metrics
                    .connections
                    .iter()
                    .take(MAX_HEALTH_CONNECTION_IDS)
                    .map(|connection| connection_id_hash(&connection.id));
                let new_connection = connection_ids
                    .clone()
                    .any(|id| !self.health.connection_ids.contains(&id));
                self.health.connection_ids.clear();
                self.health.connection_ids.extend(connection_ids);
                if new_connection
                    && !self.metrics_state.proxy_ips_in_flight
                    && !self.health.probe_in_flight
                {
                    self.health.last_activity = Some(now);
                    self.health.fast_probe_pending = true;
                }
                // Node delays live in their own map; keeping a second copy in
                // the metrics snapshot doubles the per-node bookkeeping and is
                // resent every second.
                for (tag, delay) in metrics.node_delays.drain() {
                    self.metrics_state.node_delays.insert(tag, delay);
                }
                self.metrics_state.metrics = metrics;
                if self.metrics_state.metrics.connections.is_empty() {
                    self.metrics_state.idle_since.get_or_insert(now);
                } else {
                    self.metrics_state.idle_since = None;
                }
                self.observe_selection_change(now);
                self.sort_nodes_if_visible();
            }
            AppEvent::HealthProbe {
                generation,
                tag,
                outcome,
            } => {
                if generation != self.health.generation {
                    return;
                }
                self.health.probe_in_flight = false;
                if self.proxy_runtime.status != ProxyStatus::Running {
                    self.reset_health_failures();
                    return;
                }
                match outcome {
                    HealthOutcome::Healthy(delay) => {
                        let recovered = self.health.pause_until.take().is_some()
                            || self.health.no_node_level > 0;
                        let had_failures = self.health.failures > 0;
                        self.health.no_node_level = 0;
                        self.health.failures = 0;
                        self.health.failure_tag = None;
                        if self.ui_state.last_error.as_deref()
                            == Some(self.text().msg_health_no_nodes)
                        {
                            self.ui_state.last_error = None;
                        }
                        if recovered {
                            self.logger.app(
                                LogLevel::Info,
                                format!("health restored: {tag} ({delay} ms)"),
                            );
                        } else if had_failures {
                            self.logger.app(
                                LogLevel::Info,
                                format!("health recovered: {tag} ({delay} ms)"),
                            );
                        }
                        self.health.quality.record_success(&tag, delay);
                        self.metrics_state.node_delays.insert(tag, delay);
                        self.sort_nodes_if_visible();
                    }
                    HealthOutcome::Failed => {
                        if self.health_target().as_deref() != Some(tag.as_str()) {
                            self.reset_health_failures();
                            return;
                        }
                        self.stats.health_probe_failures_total =
                            self.stats.health_probe_failures_total.saturating_add(1);
                        if self.health.failure_tag.as_deref() != Some(tag.as_str()) {
                            self.health.failure_tag = Some(tag.clone());
                            self.health.failures = 0;
                        }
                        self.health.failures += 1;
                        self.health.quality.record_failure(&tag);
                        if self.health.failures == 1 {
                            self.logger.app(
                                LogLevel::Info,
                                format!("health degraded: {tag} (1/{HEALTH_FAILURE_THRESHOLD})"),
                            );
                        }
                        if self.health.failures >= HEALTH_FAILURE_THRESHOLD
                            && !self.health.recovery_in_flight
                            && !self.health.recovery_refresh_pending
                            && self
                                .health
                                .last_recovery
                                .is_none_or(|at| at.elapsed() >= HEALTH_RECOVERY_COOLDOWN)
                        {
                            self.start_recovery(tag);
                        }
                    }
                    HealthOutcome::Unavailable => {}
                }
            }
            AppEvent::RecoveryProbe {
                generation,
                trigger,
                healthy,
            } => {
                if generation != self.health.generation {
                    return;
                }
                self.health.recovery_in_flight = false;
                if self.proxy_runtime.status != ProxyStatus::Running
                    || self.proxy_runtime.proxy.is_none()
                {
                    self.stats.recovery_aborted = self.stats.recovery_aborted.saturating_add(1);
                    return;
                }
                if self.health_target().as_deref() != Some(trigger.as_str()) {
                    self.stats.recovery_aborted = self.stats.recovery_aborted.saturating_add(1);
                    self.reset_health_failures();
                    self.logger.app(
                        LogLevel::Info,
                        "health failover: selection changed before recovery completed",
                    );
                    return;
                }
                let Some(target) = choose_recovery_target(
                    self.metrics_state.metrics.selection.selected.as_deref(),
                    &healthy,
                    &self.health.quality,
                ) else {
                    self.on_no_healthy_nodes();
                    return;
                };
                let Some(proxy) = self.proxy_runtime.proxy.as_ref() else {
                    self.stats.recovery_aborted = self.stats.recovery_aborted.saturating_add(1);
                    return;
                };
                self.stats.recovery_selected = self.stats.recovery_selected.saturating_add(1);
                let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
                let (tag, name) = match &target {
                    RecoveryTarget::Auto => {
                        self.preferred_node = None;
                        (tags::AUTO.to_string(), String::new())
                    }
                    RecoveryTarget::Node(tag) => {
                        let name = self
                            .metrics_state
                            .nodes
                            .iter()
                            .find(|node| node.tag == *tag)
                            .map(|node| node.name.clone())
                            .unwrap_or_else(|| tag.clone());
                        self.preferred_node = Some(tag.clone());
                        (tag.clone(), name)
                    }
                };
                self.pending_selection = None;
                self.record_auto_switch();
                self.set_switch_reason("recovery");
                self.selection_generation.fetch_add(1, Ordering::Relaxed);
                self.health.failures = 0;
                self.health.failure_tag = None;
                self.health.no_node_level = 0;
                self.health.pause_until = None;
                self.ui_state.last_error = None;
                self.logger.app(
                    LogLevel::Warn,
                    format!("health failover: {trigger} -> {tag}"),
                );
                let label = match target {
                    RecoveryTarget::Auto => self.text().msg_health_auto.to_string(),
                    RecoveryTarget::Node(_) => {
                        format!("{}: {name}", self.text().msg_health_switched)
                    }
                };
                let tx = self.events_tx.clone();
                let generation = self.selection_generation.load(Ordering::Relaxed);
                let token = self.selection_generation.clone();
                self.spawn_guarded(BackgroundTask::Selection, async move {
                    if token.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    match client.select(&tag).await {
                        Ok(()) => {
                            if token.load(Ordering::Relaxed) == generation {
                                let _ = tx.send(AppEvent::Info(label));
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(AppEvent::Error(err.to_string()));
                        }
                    }
                });
            }
            AppEvent::ProxyIps { ips, errors } => {
                self.metrics_state.proxy_ips_in_flight = false;
                if self.proxy_runtime.proxy.is_none() {
                    return;
                }
                if ips.is_empty() {
                    self.metrics_state.proxy_ips.clear();
                    self.metrics_state.proxy_ip_backoff_secs =
                        (self.metrics_state.proxy_ip_backoff_secs * 2).min(60);
                    let in_grace = self
                        .proxy_runtime
                        .started_at
                        .is_some_and(|at| at.elapsed() < PROXY_IP_STARTUP_GRACE);
                    if !in_grace {
                        self.metrics_state.proxy_ip_failures =
                            self.metrics_state.proxy_ip_failures.saturating_add(1);
                    }
                    self.logger.app(
                        proxy_ip_failure_level(self.metrics_state.proxy_ip_failures, in_grace),
                        format!("proxy IP lookup failed: {}", errors.join("; ")),
                    );
                } else {
                    self.metrics_state.proxy_ips = ips;
                    self.metrics_state.proxy_ip_backoff_secs = 5;
                    self.metrics_state.proxy_ip_failures = 0;
                }
            }
            AppEvent::NodeDelay { tag, delay } => {
                self.metrics_state.delay_testing.remove(&tag);
                if self.proxy_runtime.proxy.is_none() {
                    return;
                }
                if let Some(delay) = delay.filter(|delay| *delay > 0) {
                    self.health.quality.record_success(&tag, delay);
                    self.metrics_state.node_delays.insert(tag, delay);
                    self.sort_nodes_if_visible();
                } else {
                    self.health.quality.record_failure(&tag);
                }
            }
            AppEvent::NodeSelected(tag) => {
                if self.metrics_state.nodes.iter().any(|node| node.tag == tag) {
                    if self.metrics_state.metrics.selection.selected.as_deref()
                        != Some(tag.as_str())
                    {
                        self.set_switch_reason("manual");
                    }
                    self.preferred_node = Some(tag);
                }
            }
            AppEvent::SelectionApplied => self.request_metrics_refresh(),
            AppEvent::NodeDelaySweepDone => {
                self.metrics_state.delay_sweep_running = false;
                self.metrics_state.delay_testing.clear();
                self.metrics_state.last_delay_sweep = Some(Instant::now());
            }
            AppEvent::DashboardViewport(max_scroll) => {
                self.ui_state.dashboard_max_scroll = max_scroll;
                self.ui_state.dashboard_scroll = self.ui_state.dashboard_scroll.min(max_scroll);
            }
            AppEvent::HelpViewport(max_scroll) => {
                self.ui_state.help_max_scroll = max_scroll;
                self.ui_state.help_scroll = self.ui_state.help_scroll.min(max_scroll);
            }
            AppEvent::LogDetailViewport(max_scroll) => {
                self.ui_state.log_detail_max_scroll = max_scroll;
                if let Some(Overlay::LogDetail { scroll, .. }) = self.ui_state.overlay.as_mut() {
                    *scroll = (*scroll).min(max_scroll);
                }
            }
            AppEvent::UpdateCheckDone(result) => {
                self.binary_state.update_check_in_flight = false;
                match result {
                    Ok(version) => {
                        self.logger.app(
                            LogLevel::Info,
                            format!("latest sing-box release: {version}"),
                        );
                        self.binary_state.latest_binary_version = Some(version);
                    }
                    Err(err) => {
                        self.logger.app(
                            LogLevel::Debug,
                            format!("sing-box update check failed: {err}"),
                        );
                    }
                }
            }
            AppEvent::RollbackDone(result) => {
                self.binary_state.setup.rollback_in_flight = false;
                self.ui_state.rollback_confirm = false;
                match result {
                    Ok(metadata) => {
                        self.binary_state.binary_version = Some(metadata.version.clone());
                        self.binary_state.binary_verified = Some(metadata.verified);
                        self.binary_state.binary_installed = true;
                        self.binary_state.setup.message = None;
                        self.refresh_previous_binary_state();
                        self.notify(format!(
                            "{} {}",
                            self.text().msg_binary_switched,
                            metadata.version
                        ));
                        self.logger.app(
                            LogLevel::Info,
                            format!("switched sing-box binary to {}", metadata.version),
                        );
                        self.auto_restart_after_change();
                    }
                    Err(err) => {
                        self.binary_state.setup.message =
                            Some(format!("{}: {err}", self.text().setup_switch_failed));
                        self.ui_state.last_error = Some(err);
                    }
                }
            }
            AppEvent::Info(message) => {
                self.notify(message.clone());
                self.logger.app(LogLevel::Info, message);
            }
            AppEvent::Error(message) => {
                self.ui_state.last_error = Some(message.clone());
                self.logger.app(LogLevel::Error, message);
            }
            AppEvent::ClientAttached {
                id,
                viewer,
                tx,
                accept,
            } => {
                if self.clients.len() >= MAX_CLIENTS
                    || (viewer && self.clients.values().any(|client| client.viewer))
                {
                    let _ = tx.try_send(ServerMsg::Busy);
                    let _ = accept.send(false);
                } else {
                    self.clients.insert(id, ClientHandle { viewer, tx });
                    let _ = accept.send(true);
                    if viewer {
                        self.metrics_state
                            .viewer_active
                            .store(true, Ordering::Relaxed);
                        self.metrics_state.metrics_wake.notify_one();
                        if self.ui_state.page == Page::Logs && self.ui_state.log_follow {
                            self.ui_state.selected = self.log_count().saturating_sub(1);
                        }
                        self.sort_nodes_if_visible();
                        self.broadcast_view();
                    }
                }
            }
            AppEvent::ClientDetached(id) => {
                if let Some(client) = self.clients.remove(&id) {
                    if client.viewer {
                        self.metrics_state
                            .viewer_active
                            .store(false, Ordering::Relaxed);
                    }
                }
            }
            AppEvent::StartProxyRequest { client_id } => {
                self.handle_start_request(client_id);
            }
            AppEvent::StatusRequest { client_id } => {
                self.respond_status(client_id);
            }
            AppEvent::ShutdownRequest { client_id } => {
                if self.clients.contains_key(&client_id) {
                    self.ack(client_id, true, "stopping");
                    self.should_quit = true;
                }
            }
            AppEvent::DaemonSignal => {
                self.should_quit = true;
            }
        }
    }

    fn ack(&self, client_id: u64, ok: bool, message: impl Into<String>) {
        if let Some(client) = self.clients.get(&client_id) {
            let _ = client.tx.try_send(ServerMsg::Ack {
                ok,
                message: message.into(),
            });
        }
    }

    fn respond_status(&self, client_id: u64) {
        let Some(client) = self.clients.get(&client_id) else {
            return;
        };
        let status = DaemonStatus {
            pid: std::process::id(),
            managed: self.managed,
            service: self.service.clone(),
            proxy: self.proxy_runtime.status.clone(),
            uptime_secs: self
                .proxy_runtime
                .started_at
                .map(|started| started.elapsed().as_secs())
                .unwrap_or(0),
            listen: self.listen_addresses(),
            current_node: self.current_node(),
            version: crate::version::DISPLAY.to_string(),
        };
        let _ = client.tx.try_send(ServerMsg::Status(Box::new(status)));
    }

    fn ack_pending_start(&mut self, ok: bool, message: impl Into<String>) {
        if let Some(client_id) = self.pending_start.take() {
            self.ack(client_id, ok, message);
        }
    }

    fn handle_start_request(&mut self, client_id: u64) {
        if !self.clients.contains_key(&client_id) {
            return;
        }
        if self.proxy_runtime.status == ProxyStatus::Running {
            self.ack(client_id, true, "proxy is already running");
            return;
        }
        if self.proxy_busy() {
            self.ack(client_id, false, "proxy state is changing");
            return;
        }
        if self.metrics_state.nodes.is_empty() {
            self.pending_start = Some(client_id);
            if self.subscription_state.refreshing_subscriptions {
                return;
            }
            if self.config.enabled_subscriptions().next().is_some() {
                self.refresh_subscriptions();
                return;
            }
            self.ack_pending_start(false, "no nodes available");
            return;
        }
        match self.start_proxy() {
            Ok(()) => self.pending_start = Some(client_id),
            Err(err) => self.ack(client_id, false, err),
        }
    }

    fn retry_pending_start(&mut self) {
        if self.pending_start.is_none() {
            return;
        }
        if self.metrics_state.nodes.is_empty() {
            if self.subscription_state.refreshing_subscriptions {
                return;
            }
            self.ack_pending_start(false, "no nodes available");
            return;
        }
        if self.proxy_busy() {
            return;
        }
        if let Err(err) = self.start_proxy() {
            self.ack_pending_start(false, err);
        }
    }

    fn broadcast(&self, message: ServerMsg) {
        for client in self.clients.values().filter(|client| client.viewer) {
            let _ = client.tx.try_send(message.clone());
        }
    }

    fn has_viewer(&self) -> bool {
        self.metrics_state.viewer_active.load(Ordering::Relaxed)
    }

    fn request_metrics_refresh(&self) {
        self.metrics_state
            .proxies_refresh
            .store(true, Ordering::Relaxed);
        self.metrics_state.metrics_wake.notify_one();
    }

    fn broadcast_view(&self) {
        if !self.has_viewer() {
            return;
        }
        let view = Box::new(self.view());
        for client in self.clients.values().filter(|client| client.viewer) {
            let _ = client.tx.try_send(ServerMsg::View(view.clone()));
        }
    }

    /// Spawn a background task that reports completion through the event
    /// channel. If the task panics, the panic is caught and surfaced as
    /// `AppEvent::BackgroundPanic` so the matching in-flight flag is cleared
    /// instead of staying set forever.
    fn spawn_guarded<F>(&self, task: BackgroundTask, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        drop(self.spawn_guarded_handle(task, fut));
    }

    fn spawn_guarded_handle<F>(&self, task: BackgroundTask, fut: F) -> JoinHandle<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let tx = self.events_tx.clone();
        let logger = self.logger.clone();
        tokio::spawn(async move {
            if let Err(payload) = crate::guard::CatchUnwind::new(fut).await {
                let message = crate::guard::panic_message(payload.as_ref());
                logger.app(
                    LogLevel::Error,
                    format!("background task panicked: {message}"),
                );
                let _ = tx.send(AppEvent::BackgroundPanic { task, message });
            }
        })
    }

    fn on_background_panic(&mut self, task: BackgroundTask, message: String) {
        match task {
            BackgroundTask::SubscriptionRefresh => {
                self.subscription_state.refreshing_subscriptions = false;
                self.health.recovery_refresh_pending = false;
                self.schedule_subscription_retry();
            }
            BackgroundTask::ProxyStart => {
                self.proxy_runtime.restart_in_progress = false;
                self.proxy_runtime.status = ProxyStatus::Failed;
                self.ack_pending_start(false, "proxy start task failed");
            }
            BackgroundTask::ProxyStop => {
                self.proxy_runtime.restart_in_progress = false;
                self.proxy_runtime.status = ProxyStatus::Stopped;
                self.proxy_runtime.restart_after_stop = false;
            }
            BackgroundTask::ProxyIps => self.metrics_state.proxy_ips_in_flight = false,
            BackgroundTask::DelaySweep => self.metrics_state.delay_sweep_running = false,
            BackgroundTask::HealthProbe => self.health.probe_in_flight = false,
            BackgroundTask::Recovery => self.health.recovery_in_flight = false,
            BackgroundTask::BinaryDownload => self.binary_state.setup.downloading = false,
            BackgroundTask::BinaryInstall => {
                self.binary_state.install_in_flight = false;
                self.binary_state.setup.installing = false;
            }
            BackgroundTask::BinaryRollback => self.binary_state.setup.rollback_in_flight = false,
            BackgroundTask::BinaryUpdateCheck => self.binary_state.update_check_in_flight = false,
            BackgroundTask::Preflight => {
                self.proxy_runtime.preflight_in_flight = false;
                self.proxy_runtime.restart_in_progress = false;
            }
            BackgroundTask::Metrics => {
                self.proxy_runtime.metrics_task = None;
            }
            BackgroundTask::DelayTest => {
                self.metrics_state.delay_testing.clear();
            }
            BackgroundTask::Selection | BackgroundTask::Ipc => {}
            BackgroundTask::Other => {
                self.subscription_state.pending_restart_subscription = None;
            }
        }
        let text = format!("background task failed: {message}");
        self.ui_state.last_error = Some(text.clone());
        self.notify(text);
    }

    fn publish_view(&mut self) {
        if self.has_viewer() {
            if self.ui_state.page == Page::Nodes {
                self.refresh_auto_tags_cache();
            }
            self.broadcast_view();
        }
    }

    pub fn notify(&mut self, message: impl Into<String>) {
        self.ui_state.message = Some(message.into());
        self.ui_state.message_at = Some(Instant::now());
    }

    fn prune_node_state(&mut self) {
        let tags: HashSet<String> = self
            .metrics_state
            .nodes
            .iter()
            .map(|node| node.tag.clone())
            .collect();
        self.metrics_state
            .node_delays
            .retain(|tag, _| tags.contains(tag));
        self.metrics_state
            .delay_testing
            .retain(|tag| tags.contains(tag));
        self.health.quality.prune(&tags);
    }

    fn sync_preferred_node(&mut self) {
        if let Some(tag) = self.preferred_node.clone() {
            if !self.metrics_state.nodes.iter().any(|node| node.tag == tag) {
                let name = self
                    .proxy_runtime
                    .proxy_node_names
                    .get(&tag)
                    .map(|name| sanitize_owned(name.clone()))
                    .filter(|name| !name.is_empty())
                    .map(|name| format!(" ({name})"))
                    .unwrap_or_default();
                self.preferred_node = None;
                self.pending_selection = None;
                self.logger.app(
                    LogLevel::Warn,
                    format!(
                        "manual node removed from subscription: {tag}{name}; falling back to auto"
                    ),
                );
                self.notify(self.text().msg_preferred_missing);
            }
        }
    }

    fn setup_busy(&self) -> bool {
        self.binary_state.setup.downloading
            || self.binary_state.setup.rollback_in_flight
            || self.binary_state.install_in_flight
    }

    fn proxy_busy(&self) -> bool {
        matches!(
            self.proxy_runtime.status,
            ProxyStatus::Starting | ProxyStatus::Stopping
        )
    }

    fn start_or_stop(&mut self) {
        if self.proxy_busy() {
            self.notify(self.text().msg_proxy_busy);
            return;
        }
        if self.proxy_runtime.proxy.is_some() {
            self.stop_proxy();
            return;
        }
        self.proxy_runtime.restart_attempts = 0;
        self.proxy_runtime.restart_at = None;
        self.proxy_runtime.stable_reset_at = None;
        let _ = self.start_proxy();
    }

    fn request_restart(&mut self) {
        if self.setup_busy() {
            self.notify(self.text().setup_busy);
            return;
        }
        if self.proxy_busy() {
            self.notify(self.text().msg_proxy_busy);
            return;
        }
        if self.proxy_runtime.status != ProxyStatus::Running {
            self.notify(self.text().msg_proxy_not_running);
            return;
        }
        self.ui_state.restart_confirm = true;
        self.ui_state.message = None;
    }

    fn perform_restart(&mut self) {
        if self.setup_busy() {
            self.notify(self.text().setup_busy);
            return;
        }
        if self.proxy_busy() {
            self.notify(self.text().msg_proxy_busy);
            return;
        }
        if self.proxy_runtime.status != ProxyStatus::Running || self.proxy_runtime.proxy.is_none() {
            self.notify(self.text().msg_proxy_not_running);
            return;
        }
        self.begin_restart();
    }

    fn auto_restart_after_change(&mut self) -> bool {
        if self.proxy_runtime.restart_in_progress {
            return false;
        }
        if self.proxy_runtime.status != ProxyStatus::Running || self.proxy_runtime.proxy.is_none() {
            return false;
        }
        if self
            .subscription_state
            .pending_restart_subscription
            .is_some()
        {
            return false;
        }
        if self.setup_busy() {
            return false;
        }
        if self.metrics_state.nodes.is_empty() {
            self.ui_state.last_error = Some(self.text().no_nodes.to_string());
            self.notify(self.text().no_nodes);
            return false;
        }
        let filtered = self.auto_candidate_tags();
        if filtered.is_empty() {
            self.ui_state.last_error = Some(self.text().no_matching_nodes.to_string());
            self.notify(self.text().no_matching_nodes);
            return false;
        }
        self.begin_restart()
    }

    fn begin_restart(&mut self) -> bool {
        if self.proxy_runtime.preflight_in_flight || self.proxy_runtime.restart_in_progress {
            return false;
        }
        // Validate the proposed configuration before tearing down the running
        // instance. The check runs on a blocking thread so a slow `sing-box
        // check` cannot stall the daemon event loop.
        self.ui_state.restart_confirm = false;
        self.ui_state.rollback_confirm = false;
        self.ui_state.subscription_delete = None;
        self.proxy_runtime.preflight_in_flight = true;
        self.proxy_runtime.restart_in_progress = true;
        self.notify(self.text().msg_restarting);
        let paths = self.paths.clone();
        let config = self.config.clone();
        let nodes = self.metrics_state.nodes.clone();
        let preferred = self.preferred_node.clone();
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::Preflight, async move {
            let result = tokio::task::spawn_blocking(move || {
                supervisor::preflight(&paths, &config, &nodes, preferred.as_deref())
                    .map_err(|err| err.to_string())
            })
            .await
            .unwrap_or_else(|err| Err(format!("preflight task failed: {err}")));
            let _ = tx.send(AppEvent::PreflightDone(result));
        });
        true
    }

    fn finish_restart_after_preflight(&mut self, result: Result<(), String>) {
        self.proxy_runtime.preflight_in_flight = false;
        match result {
            Ok(()) => {
                if self.proxy_runtime.proxy.is_some() {
                    self.proxy_runtime.restart_after_stop = true;
                    self.stop_proxy();
                } else {
                    // The instance disappeared while the check was running;
                    // avoid leaving the busy flag set with nothing to restart.
                    self.proxy_runtime.restart_in_progress = false;
                    let _ = self.start_proxy();
                }
            }
            Err(err) => {
                self.proxy_runtime.restart_in_progress = false;
                let message = format!("{}: {err}", self.text().msg_preflight_failed);
                self.ui_state.last_error = Some(message.clone());
                self.logger.app(LogLevel::Warn, message);
                self.notify(self.text().msg_preflight_failed);
                self.ui_state.restart_confirm = false;
                self.ui_state.rollback_confirm = false;
            }
        }
    }

    fn start_proxy(&mut self) -> Result<(), String> {
        if self.proxy_busy() {
            return Err(self.text().msg_proxy_busy.to_string());
        }
        if self.binary_manager.locate().is_none() {
            self.open_setup_overlay();
            self.ui_state.last_error = Some(self.text().msg_singbox_missing.to_string());
            return Err(self.text().msg_singbox_missing.to_string());
        }
        if self.metrics_state.nodes.is_empty() {
            self.ui_state.last_error = Some(self.text().no_nodes.to_string());
            self.switch_page(Page::Subscriptions);
            return Err(self.text().no_nodes.to_string());
        }
        let filtered = self.auto_candidate_tags();
        if filtered.is_empty() {
            self.ui_state.last_error = Some(self.text().no_matching_nodes.to_string());
            return Err(self.text().no_matching_nodes.to_string());
        }
        self.proxy_runtime.runtime_has_auto = filtered.len() >= 2;
        let mut preferred = self.preferred_node.clone();
        if let Some(tag) = preferred.as_deref() {
            if !self.metrics_state.nodes.iter().any(|node| node.tag == tag) {
                preferred = None;
                self.preferred_node = None;
                self.pending_selection = None;
                self.notify(self.text().msg_preferred_missing);
            }
        }
        self.proxy_runtime.proxy_node_tags = self
            .metrics_state
            .nodes
            .iter()
            .map(|node| node.tag.clone())
            .collect();
        self.proxy_runtime.proxy_node_names = self
            .metrics_state
            .nodes
            .iter()
            .map(|node| (node.tag.clone(), node.name.clone()))
            .collect();
        self.proxy_runtime.status = ProxyStatus::Starting;
        let paths = self.paths.clone();
        let config = self.config.clone();
        let nodes = self.metrics_state.nodes.clone();
        let logger = self.logger.clone();
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::ProxyStart, async move {
            let result = supervisor::start(&paths, &config, &nodes, &logger, preferred.as_deref())
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::ProxyStarted(result));
        });
        Ok(())
    }

    fn schedule_restart(&mut self) {
        if self.proxy_runtime.restart_attempts >= MAX_RESTART_ATTEMPTS {
            self.proxy_runtime.restart_at = None;
            self.notify(self.text().msg_restart_paused);
            self.logger
                .app(LogLevel::Error, "restart paused after 5 failures");
            return;
        }
        let delay = RESTART_DELAYS[self.proxy_runtime.restart_attempts as usize];
        self.proxy_runtime.restart_attempts += 1;
        self.proxy_runtime.restart_at = Some(Instant::now() + Duration::from_secs(delay));
        self.notify(format!(
            "{}: {}s (#{})",
            self.text().msg_restart_scheduled,
            delay,
            self.proxy_runtime.restart_attempts
        ));
        self.logger.app(
            LogLevel::Warn,
            format!(
                "restart scheduled in {delay}s (attempt {}/{MAX_RESTART_ATTEMPTS})",
                self.proxy_runtime.restart_attempts
            ),
        );
    }

    fn stop_proxy(&mut self) {
        if let Some(task) = self.proxy_runtime.metrics_task.take() {
            task.abort();
        }
        self.proxy_runtime.restart_at = None;
        self.proxy_runtime.restart_attempts = 0;
        self.proxy_runtime.stable_reset_at = None;
        self.reset_metrics();
        if let Some(proxy) = self.proxy_runtime.proxy.take() {
            self.proxy_runtime.status = ProxyStatus::Stopping;
            let logger = self.logger.clone();
            let tx = self.events_tx.clone();
            self.spawn_guarded(BackgroundTask::ProxyStop, async move {
                supervisor::stop(proxy, &logger).await;
                let _ = tx.send(AppEvent::ProxyStopped);
            });
        } else {
            self.proxy_runtime.status = ProxyStatus::Stopped;
        }
    }

    fn start_metrics(&mut self, port: u16, secret: String) {
        if let Some(task) = self.proxy_runtime.metrics_task.take() {
            task.abort();
        }
        let tx = self.events_tx.clone();
        let client = ClashClient::new(port, secret);
        self.logger.app(
            LogLevel::Info,
            format!("{}: {}", self.text().msg_clash_api, client.base_url()),
        );
        let clash_api_label = self.text().msg_clash_api;
        let viewer_active = self.metrics_state.viewer_active.clone();
        let metrics_wake = self.metrics_state.metrics_wake.clone();
        let proxies_refresh = self.metrics_state.proxies_refresh.clone();
        self.proxy_runtime.metrics_task = Some(self.spawn_guarded_handle(
            BackgroundTask::Metrics,
            async move {
                let mut selection = ProxySelection::default();
                let mut pending_node_delays = HashMap::new();
                let mut next_proxies = Instant::now();
                let mut last_connection_error: Option<String> = None;
                let mut last_proxy_error: Option<String> = None;
                loop {
                    let connections = match client.connections().await {
                        Ok(snapshot) => {
                            last_connection_error = None;
                            Some(snapshot)
                        }
                        Err(err) => {
                            report_clash_error(
                                &tx,
                                clash_api_label,
                                &mut last_connection_error,
                                err,
                            );
                            None
                        }
                    };
                    if Instant::now() >= next_proxies
                        || proxies_refresh.swap(false, Ordering::Relaxed)
                    {
                        match client.proxies().await {
                            Ok(snapshot) => {
                                selection = snapshot.selection;
                                // Publish the delay map once per proxies poll;
                                // the app merges it into its own copy.
                                pending_node_delays = snapshot.node_delays;
                                last_proxy_error = None;
                            }
                            Err(err) => {
                                report_clash_error(
                                    &tx,
                                    clash_api_label,
                                    &mut last_proxy_error,
                                    err,
                                );
                            }
                        }
                        next_proxies = Instant::now() + PROXIES_REFRESH_INTERVAL;
                    }
                    let mut interval = Duration::from_secs(1);
                    if let Some(connections) = connections {
                        let idle = connections.connections.is_empty();
                        let metrics = MetricsSnapshot {
                            upload_total: connections.upload_total,
                            download_total: connections.download_total,
                            connections: connections.connections,
                            selection: selection.clone(),
                            node_delays: std::mem::take(&mut pending_node_delays),
                        };
                        if tx.send(AppEvent::Metrics(metrics)).is_err() {
                            break;
                        }
                        if idle && !viewer_active.load(Ordering::Relaxed) {
                            interval = IDLE_METRICS_INTERVAL;
                        }
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(interval) => {}
                        _ = metrics_wake.notified() => {}
                    }
                }
            },
        ));
    }

    fn refresh_proxy_ips(&mut self) {
        if self.proxy_runtime.proxy.is_none() || self.metrics_state.proxy_ips_in_flight {
            return;
        }
        self.metrics_state.proxy_ips_in_flight = true;
        self.metrics_state.last_proxy_ips = Instant::now();
        let tx = self.events_tx.clone();
        let v4_url = self.config.proxy_ip_v4_url.clone();
        let v6_url = self.config.proxy_ip_v6_url.clone();
        let proxy_url = match self
            .proxy_runtime
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.lan_auth.as_ref())
        {
            Some((username, password)) => netinfo::local_proxy_url_with_auth(
                self.proxy_runtime
                    .proxy
                    .as_ref()
                    .map_or(self.config.port, |proxy| proxy.port),
                username,
                password,
            ),
            None => netinfo::local_proxy_url(
                self.proxy_runtime
                    .proxy
                    .as_ref()
                    .map_or(self.config.port, |proxy| proxy.port),
            ),
        };
        self.spawn_guarded(BackgroundTask::ProxyIps, async move {
            let (v4, v6) = tokio::join!(
                netinfo::fetch_proxy_ip(&v4_url, &proxy_url),
                netinfo::fetch_proxy_ip(&v6_url, &proxy_url)
            );
            let mut ips = Vec::new();
            let mut errors = Vec::new();
            match v4 {
                Ok(ip) => ips.push(format!("v4 {ip}")),
                Err(err) => errors.push(format!("v4 {err}")),
            }
            match v6 {
                Ok(ip) => ips.push(format!("v6 {ip}")),
                Err(err) => errors.push(format!("v6 {err}")),
            }
            let _ = tx.send(AppEvent::ProxyIps { ips, errors });
        });
    }

    async fn select_node(&mut self) {
        let Some(node) = self
            .metrics_state
            .nodes
            .get(self.ui_state.selected)
            .cloned()
        else {
            return;
        };
        if self.proxy_busy() {
            self.ui_state.last_error = Some(self.text().msg_proxy_busy.to_string());
            return;
        }
        if self.proxy_runtime.proxy.is_none() {
            self.preferred_node = Some(node.tag.clone());
            self.pending_selection = Some(PendingSelection::Node(node.tag.clone()));
            self.notify(format!(
                "{}: {}",
                self.text().msg_node_selected_pending,
                node.name
            ));
            return;
        }
        self.selection_generation.fetch_add(1, Ordering::Relaxed);
        if !self.proxy_runtime.proxy_node_tags.is_empty()
            && !self.proxy_runtime.proxy_node_tags.contains(&node.tag)
        {
            self.preferred_node = Some(node.tag.clone());
            self.pending_selection = None;
            let restarting = self.auto_restart_after_change();
            self.notify(if restarting {
                self.text().msg_node_switching
            } else {
                self.text().msg_node_not_in_runtime
            });
            return;
        }
        let Some(client) = self
            .proxy_runtime
            .proxy
            .as_ref()
            .map(|proxy| ClashClient::new(proxy.clash.port, proxy.clash.secret.clone()))
        else {
            return;
        };
        let tag = node.tag.clone();
        let name = node.name.clone();
        self.pending_selection = None;
        self.invalidate_health_tasks();
        self.request_metrics_refresh();
        self.notify(format!("{}: {name}", self.text().msg_node_selected));
        let tx = self.events_tx.clone();
        let selected_label = self.text().msg_node_selected;
        let generation = self.selection_generation.load(Ordering::Relaxed);
        let token = self.selection_generation.clone();
        self.spawn_guarded(BackgroundTask::Selection, async move {
            if token.load(Ordering::Relaxed) != generation {
                return;
            }
            match client.select(&tag).await {
                Ok(()) => {
                    if token.load(Ordering::Relaxed) == generation {
                        let _ = tx.send(AppEvent::NodeSelected(tag));
                        let _ = tx.send(AppEvent::Info(format!("{selected_label}: {name}")));
                        let _ = tx.send(AppEvent::SelectionApplied);
                    }
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::Error(err.to_string()));
                }
            }
        });
    }

    fn apply_pending_selection(&mut self, port: u16, secret: String) {
        let Some(pending) = self.pending_selection.take() else {
            return;
        };
        let (target, label) = match pending {
            PendingSelection::Auto => {
                let Some(mut target) = self.auto_selection_target() else {
                    let message = self.text().no_matching_nodes.to_string();
                    let _ = self.events_tx.send(AppEvent::Error(message));
                    return;
                };
                if target == tags::AUTO && !self.proxy_runtime.runtime_has_auto {
                    target = self
                        .auto_candidate_tags()
                        .first()
                        .cloned()
                        .unwrap_or(target);
                }
                (target, self.text().msg_auto_enabled.to_string())
            }
            PendingSelection::Node(tag) => {
                let name = self
                    .metrics_state
                    .nodes
                    .iter()
                    .find(|node| node.tag == tag)
                    .map(|node| node.name.clone())
                    .unwrap_or_else(|| tag.clone());
                (tag, format!("{}: {name}", self.text().msg_node_selected))
            }
        };
        self.request_metrics_refresh();
        let tx = self.events_tx.clone();
        let generation = self.selection_generation.load(Ordering::Relaxed);
        let token = self.selection_generation.clone();
        self.spawn_guarded(BackgroundTask::Selection, async move {
            let client = ClashClient::new(port, secret);
            for attempt in 0..20 {
                if token.load(Ordering::Relaxed) != generation {
                    return;
                }
                match client.select(&target).await {
                    Ok(()) => {
                        if token.load(Ordering::Relaxed) == generation {
                            let _ = tx.send(AppEvent::Info(label));
                            let _ = tx.send(AppEvent::SelectionApplied);
                        }
                        return;
                    }
                    Err(_) if attempt < 19 => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    Err(err) => {
                        let _ = tx.send(AppEvent::Error(err.to_string()));
                    }
                }
            }
        });
    }

    fn test_node_delay(&mut self) {
        let Some(node) = self.metrics_state.nodes.get(self.ui_state.selected) else {
            return;
        };
        let Some(proxy) = self.proxy_runtime.proxy.as_ref() else {
            self.ui_state.last_error = Some(self.text().msg_proxy_not_running.to_string());
            return;
        };
        let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
        let tag = node.tag.clone();
        self.metrics_state.delay_testing.insert(tag.clone());
        let test_url = self.config.test_url.clone();
        let label = self.text().msg_node_delay;
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::DelayTest, async move {
            match client.delay(&tag, &test_url).await {
                Ok(delay) => {
                    let _ = tx.send(AppEvent::NodeDelay {
                        tag: tag.clone(),
                        delay: Some(delay),
                    });
                    let _ = tx.send(AppEvent::Info(format!("{label}: {tag} {delay} ms")));
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::NodeDelay { tag, delay: None });
                    let _ = tx.send(AppEvent::Error(err.to_string()));
                }
            }
        });
    }

    fn maybe_test_node_delays(&mut self, force: bool) {
        if self.proxy_runtime.proxy.is_none() || self.metrics_state.delay_sweep_running {
            return;
        }
        if !force
            && self
                .metrics_state
                .last_delay_sweep
                .is_some_and(|at| at.elapsed() < DELAY_SWEEP_COOLDOWN)
        {
            return;
        }
        let Some(proxy) = self.proxy_runtime.proxy.as_ref() else {
            return;
        };
        let tags: Vec<String> = self
            .metrics_state
            .nodes
            .iter()
            .take(MAX_LIST_ROWS)
            .filter(|node| force || !self.metrics_state.node_delays.contains_key(&node.tag))
            .map(|node| node.tag.clone())
            .collect();
        if tags.is_empty() {
            return;
        }
        let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
        let test_url = self.config.test_url.clone();
        let tx = self.events_tx.clone();
        self.metrics_state.delay_sweep_running = true;
        self.metrics_state.last_delay_sweep = Some(Instant::now());
        self.metrics_state.delay_testing = tags.iter().cloned().collect();
        self.spawn_guarded(BackgroundTask::DelaySweep, async move {
            for chunk in tags.chunks(DELAY_TEST_CONCURRENCY) {
                let mut tasks = JoinSet::new();
                for tag in chunk.iter().cloned() {
                    let client = client.clone();
                    let test_url = test_url.clone();
                    tasks.spawn(async move {
                        let delay = client.delay(&tag, &test_url).await.ok();
                        (tag, delay)
                    });
                }
                while let Some(result) = tasks.join_next().await {
                    if let Ok((tag, delay)) = result {
                        let _ = tx.send(AppEvent::NodeDelay { tag, delay });
                    }
                }
            }
            let _ = tx.send(AppEvent::NodeDelaySweepDone);
        });
    }

    /// Signature of the inputs that affect the automatic candidate set. The
    /// cache is keyed by value rather than by call site, so direct node or
    /// keyword changes cannot leave a stale result behind.
    fn auto_tags_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.metrics_state.nodes.len().hash(&mut hasher);
        for node in &self.metrics_state.nodes {
            node.tag.hash(&mut hasher);
            node.name.hash(&mut hasher);
            node.source.hash(&mut hasher);
        }
        self.config.keywords_include.hash(&mut hasher);
        self.config.keywords_exclude.hash(&mut hasher);
        hasher.finish()
    }

    fn refresh_auto_tags_cache(&mut self) {
        let signature = self.auto_tags_signature();
        if self
            .metrics_state
            .auto_tags_cache
            .as_ref()
            .is_some_and(|(cached, _)| *cached == signature)
        {
            return;
        }
        let tags = crate::selection::filter_node_tags(
            &self.metrics_state.nodes,
            &self.config.keywords_include,
            &self.config.keywords_exclude,
        );
        self.metrics_state.auto_tags_cache = Some((signature, tags));
    }

    /// Tags of the nodes eligible for automatic selection under the current
    /// keyword filters. Centralised so every caller uses the same predicate.
    fn auto_candidate_tags(&self) -> Vec<String> {
        let signature = self.auto_tags_signature();
        if let Some((cached, tags)) = &self.metrics_state.auto_tags_cache {
            if *cached == signature {
                return tags.clone();
            }
        }
        crate::selection::filter_node_tags(
            &self.metrics_state.nodes,
            &self.config.keywords_include,
            &self.config.keywords_exclude,
        )
    }

    fn auto_selection_target(&self) -> Option<String> {
        let filtered = self.auto_candidate_tags();
        if filtered.len() >= 2 {
            Some(tags::AUTO.to_string())
        } else {
            filtered.first().cloned()
        }
    }

    fn select_node_auto(&mut self) {
        if self.proxy_busy() {
            self.ui_state.last_error = Some(self.text().msg_proxy_busy.to_string());
            return;
        }
        if self.proxy_runtime.proxy.is_none() {
            self.preferred_node = None;
            self.pending_selection = Some(PendingSelection::Auto);
            self.notify(self.text().msg_auto_pending);
            return;
        }
        self.selection_generation.fetch_add(1, Ordering::Relaxed);
        let Some(target) = self.auto_selection_target() else {
            self.ui_state.last_error = Some(self.text().no_matching_nodes.to_string());
            return;
        };
        // A running config built with fewer than two auto candidates has no
        // urltest group. If a refresh added candidates, restart to regenerate
        // the topology instead of sending a selector request for a missing
        // "auto" child.
        if target == tags::AUTO && !self.proxy_runtime.runtime_has_auto {
            self.preferred_node = None;
            self.pending_selection = Some(PendingSelection::Auto);
            if self.auto_restart_after_change() {
                self.notify(self.text().msg_auto_restarting);
            } else {
                self.pending_selection = None;
            }
            return;
        }
        let Some(client) = self
            .proxy_runtime
            .proxy
            .as_ref()
            .map(|proxy| ClashClient::new(proxy.clash.port, proxy.clash.secret.clone()))
        else {
            return;
        };
        self.preferred_node = None;
        self.pending_selection = None;
        self.invalidate_health_tasks();
        self.request_metrics_refresh();
        self.set_switch_reason("manual");
        let tx = self.events_tx.clone();
        let auto_label = self.text().msg_auto_enabled;
        let generation = self.selection_generation.load(Ordering::Relaxed);
        let token = self.selection_generation.clone();
        self.spawn_guarded(BackgroundTask::Selection, async move {
            if token.load(Ordering::Relaxed) != generation {
                return;
            }
            match client.select(&target).await {
                Ok(()) => {
                    if token.load(Ordering::Relaxed) == generation {
                        let _ = tx.send(AppEvent::Info(auto_label.to_string()));
                        let _ = tx.send(AppEvent::SelectionApplied);
                    }
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::Error(err.to_string()));
                }
            }
        });
    }

    fn reset_metrics(&mut self) {
        self.metrics_state.up_speed = 0.0;
        self.metrics_state.down_speed = 0.0;
        self.metrics_state.last_totals = None;
        self.metrics_state.metrics = MetricsSnapshot::default();
        self.metrics_state.proxy_ips.clear();
        self.ui_state.last_node_sort = None;
        self.sort_nodes_if_visible();
        self.metrics_state.proxy_ips_in_flight = false;
        self.metrics_state.proxy_ip_backoff_secs = 5;
        self.metrics_state.node_delays.clear();
        self.metrics_state.delay_testing.clear();
        self.metrics_state.delay_sweep_running = false;
        self.metrics_state.last_delay_sweep = None;
        self.subscription_state.pending_refresh_apply = None;
        self.reset_health_state();
    }

    fn toggle_language(&mut self) {
        let mut config = self.config.clone();
        config.language = config.language.toggle();
        if let Err(err) = config.save(&self.paths.config_file) {
            self.ui_state.last_error = Some(err.to_string());
        } else {
            self.config = config;
            self.notify(self.text().settings_saved);
        }
    }

    pub fn toggle_allow_lan(&mut self) {
        let mut config = self.config.clone();
        config.allow_lan = !config.allow_lan;
        if let Err(err) = config.save(&self.paths.config_file) {
            self.ui_state.last_error = Some(err.to_string());
            return;
        }
        self.config = config;
        self.ui_state.selected = self.ui_state.selected.min(
            settings_items(self.config.allow_lan)
                .len()
                .saturating_sub(1),
        );
        let message = if self.config.allow_lan {
            self.lan_warning().to_string()
        } else {
            self.text().settings_saved.to_string()
        };
        self.notify(message);
        self.logger.app(
            if self.config.allow_lan {
                LogLevel::Warn
            } else {
                LogLevel::Info
            },
            format!(
                "LAN access set to {} (auth={})",
                self.config.allow_lan,
                self.config.lan_auth().is_some()
            ),
        );
        self.auto_restart_after_change();
    }

    fn lan_warning(&self) -> &'static str {
        if self.config.lan_auth().is_some() {
            self.text().settings_lan_auth_warning
        } else {
            self.text().settings_lan_warning
        }
    }

    async fn on_tick(&mut self) -> bool {
        let mut changed = false;
        if self
            .ui_state
            .message_at
            .is_some_and(|at| at.elapsed() >= TOAST_TTL)
        {
            self.ui_state.message = None;
            self.ui_state.message_at = None;
            changed = true;
        }
        let mut crashed = None;
        if let Some(proxy) = self.proxy_runtime.proxy.as_mut() {
            if let Ok(Some(status)) = proxy.child.try_wait() {
                crashed = Some(status);
            }
        }
        if let Some(status) = crashed {
            self.proxy_runtime.proxy = None;
            if let Some(task) = self.proxy_runtime.metrics_task.take() {
                task.abort();
            }
            self.proxy_runtime.status = ProxyStatus::Failed;
            self.proxy_runtime.runtime_has_auto = false;
            self.proxy_runtime.stable_reset_at = None;
            self.proxy_runtime.proxy_node_tags.clear();
            self.proxy_runtime.proxy_node_names.clear();
            self.ui_state.last_error =
                Some(format!("{}: {status}", self.text().msg_singbox_exited));
            self.proxy_runtime.started_at = None;
            self.reset_metrics();
            self.logger.app(
                LogLevel::Error,
                format!("sing-box exited unexpectedly: {status}"),
            );
            self.schedule_restart();
            changed = true;
        }
        if self.proxy_runtime.proxy.is_some()
            && self
                .proxy_runtime
                .stable_reset_at
                .is_some_and(|at| Instant::now() >= at)
        {
            self.proxy_runtime.restart_attempts = 0;
            self.proxy_runtime.stable_reset_at = None;
        }
        if self.has_viewer() && self.ui_state.page == Page::Logs && self.ui_state.log_follow {
            self.ui_state.selected = self.log_count().saturating_sub(1);
        }
        if self.metrics_state.last_local_ips.elapsed() >= Duration::from_secs(30) {
            self.metrics_state.local_ips = netinfo::local_addresses(false);
            self.metrics_state.last_local_ips = Instant::now();
            changed = true;
        }
        if self.proxy_runtime.proxy.is_some() && !self.metrics_state.proxy_ips_in_flight {
            let interval = if self.metrics_state.proxy_ips.is_empty() {
                Duration::from_secs(self.metrics_state.proxy_ip_backoff_secs)
            } else {
                Duration::from_secs(300)
            };
            if self.metrics_state.last_proxy_ips.elapsed() >= interval {
                self.refresh_proxy_ips();
            }
        }
        self.maybe_probe_health();
        self.maybe_apply_subscription_changes();
        if self.subscription_refresh_due() {
            self.refresh_subscriptions();
            changed = true;
        }
        if self.proxy_runtime.status == ProxyStatus::Failed
            && self.proxy_runtime.proxy.is_none()
            && !self.proxy_runtime.restart_in_progress
            && !self.proxy_runtime.preflight_in_flight
            && self
                .proxy_runtime
                .restart_at
                .is_some_and(|at| Instant::now() >= at)
        {
            self.proxy_runtime.restart_at = None;
            if self.start_proxy().is_err() {
                // A synchronous failure (no matching nodes, missing binary, ...)
                // must not silently end the scheduled retry loop.
                self.schedule_restart();
            }
            changed = true;
        }
        self.logger.flush();
        changed
    }

    pub fn current_node(&self) -> Option<String> {
        let active = self
            .metrics_state
            .metrics
            .selection
            .selected
            .as_deref()
            .map(|selected| {
                if selected == tags::AUTO {
                    self.metrics_state
                        .metrics
                        .selection
                        .auto_now
                        .clone()
                        .unwrap_or_else(|| tags::AUTO.to_string())
                } else {
                    selected.to_string()
                }
            });
        let fallback = self.preferred_node.clone();
        match active.or(fallback) {
            Some(tag) if tag == tags::AUTO => Some(self.text().common_auto.to_string()),
            Some(tag) => Some(self.node_display_name(&tag)),
            None if !self.metrics_state.nodes.is_empty() => {
                Some(self.text().common_auto.to_string())
            }
            None => None,
        }
    }

    /// Whether nodes are picked automatically (urltest) or pinned manually.
    ///
    /// While running the selector state is authoritative; before startup the
    /// pending preferred node decides the mode.
    pub fn selection_mode_auto(&self) -> bool {
        match self.metrics_state.metrics.selection.selected.as_deref() {
            Some(selected) => selected == tags::AUTO,
            None => self.preferred_node.is_none(),
        }
    }

    fn node_display_name(&self, tag: &str) -> String {
        if let Some(node) = self.metrics_state.nodes.iter().find(|node| node.tag == tag) {
            return sanitize_display(&node.name);
        }
        if let Some(name) = self.proxy_runtime.proxy_node_names.get(tag) {
            return format!(
                "{}{}",
                sanitize_display(name),
                self.text().node_removed_suffix
            );
        }
        self.text().node_unknown.to_string()
    }

    pub fn proxy_pid(&self) -> u32 {
        self.proxy_runtime
            .proxy
            .as_ref()
            .map(|proxy| proxy.pid)
            .unwrap_or(0)
    }

    pub fn uptime(&self) -> String {
        let Some(started) = self.proxy_runtime.started_at else {
            return self.text().common_none.to_string();
        };
        let seconds = started.elapsed().as_secs();
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    }

    pub fn listen_addresses(&self) -> String {
        let (v4, port, ipv6) = match self.proxy_runtime.proxy.as_ref() {
            Some(proxy) => (proxy.listen_v4.as_str(), proxy.port, proxy.ipv6),
            None => (
                if self.config.allow_lan {
                    "0.0.0.0"
                } else {
                    "127.0.0.1"
                },
                self.config.port,
                self.config.enable_ipv6,
            ),
        };
        format_listen_addresses(v4, port, ipv6)
    }
}

fn report_clash_error(
    tx: &UnboundedSender<AppEvent>,
    label: &str,
    last_error: &mut Option<String>,
    err: ClashError,
) {
    let message = format!("{label}: {err}");
    if last_error.as_deref() != Some(message.as_str()) {
        *last_error = Some(message.clone());
        let _ = tx.send(AppEvent::Info(message));
    }
}

fn proxy_ip_failure_level(failures: u32, in_startup_grace: bool) -> LogLevel {
    if in_startup_grace || failures > 1 {
        LogLevel::Debug
    } else {
        LogLevel::Warn
    }
}

fn cached_subscription_age(runtimes: &[SubscriptionRuntime]) -> Option<Duration> {
    let now = chrono::Local::now().naive_local();
    let mut oldest = 0i64;
    let mut count = 0usize;
    for runtime in runtimes.iter().filter(|runtime| runtime.config.enabled) {
        count += 1;
        let updated = runtime.last_updated.as_deref().and_then(|value| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").ok()
        })?;
        oldest = oldest.max((now - updated).num_seconds().max(0));
    }
    (count > 0).then(|| Duration::from_secs(oldest as u64))
}

fn sort_nodes(
    nodes: &mut [Node],
    mode: NodeSort,
    auto_tags: &HashSet<String>,
    delays: &HashMap<String, u64>,
) {
    // Compute the sort keys once per node instead of lowercasing and probing
    // the maps on every comparison (O(n log n) allocations on each refresh).
    match mode {
        NodeSort::AutoLatency => nodes.sort_by_cached_key(|node| {
            (
                !auto_tags.contains(&node.tag),
                delays.get(&node.tag).copied().unwrap_or(u64::MAX),
                node.name.to_lowercase(),
                node.tag.clone(),
            )
        }),
        NodeSort::Name => {
            nodes.sort_by_cached_key(|node| (node.name.to_lowercase(), node.tag.clone()))
        }
    }
}

fn format_listen_addresses(v4: &str, port: u16, ipv6: bool) -> String {
    if ipv6 {
        format!("{v4}:{port} + [::1]:{port}")
    } else {
        format!("{v4}:{port}")
    }
}

fn parse_keyword_input(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for value in input.split([',', '，', ';', '；', '\n']) {
        let value = value.trim();
        if !value.is_empty() && !out.iter().any(|item| item == value) {
            out.push(value.to_string());
        }
    }
    out
}

fn empty_runtime(config: SubscriptionConfig) -> SubscriptionRuntime {
    SubscriptionRuntime {
        config,
        source_url_hash: None,
        nodes: Vec::new(),
        unsupported: Vec::new(),
        last_updated: None,
        error: None,
        stale: false,
    }
}

fn default_subscription_name(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .unwrap_or_else(|| "subscription".to_string())
}

fn spawn_auto_start(socket: PathBuf, logger: Logger) {
    tokio::spawn(async move {
        for _ in 0..80 {
            if ipc::daemon_is_running(&socket).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        match ipc::control_start(&socket).await {
            Ok(ack) if ack.ok => {
                logger.app(LogLevel::Info, "auto-start: proxy started");
            }
            Ok(ack) => {
                logger.app(LogLevel::Warn, format!("auto-start: {}", ack.message));
            }
            Err(err) => {
                logger.app(LogLevel::Warn, format!("auto-start failed: {err}"));
            }
        }
    });
}

fn spawn_signal_handler(tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut terminate = match signal(SignalKind::terminate()) {
                Ok(stream) => stream,
                Err(_) => return,
            };
            let mut interrupt = match signal(SignalKind::interrupt()) {
                Ok(stream) => stream,
                Err(_) => return,
            };
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = tx.send(AppEvent::DaemonSignal);
    });
}

#[cfg(test)]
mod tests;
