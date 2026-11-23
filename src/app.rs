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
use crate::singbox::clash::{
    ClashClient, ClashError, ConnectionsSnapshot, MetricsSnapshot, ProxiesSnapshot,
};
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
mod selection_state;
mod subscriptions;
mod ui_state;
mod updates;

use binary_state::BinaryState;
use health::HealthState;
use metrics_state::MetricsState;
use proxy::{ProxyRuntime, StartPlan};
use quality::QualityBook;
use selection_state::{PendingSelection, SelectionState};
use subscriptions::SubscriptionState;
use ui_state::{NodeSort, UiState};

const MAX_RESTART_ATTEMPTS: u32 = 5;
const RESTART_DELAYS: [u64; MAX_RESTART_ATTEMPTS as usize] = [1, 2, 5, 10, 30];
const MAX_PREFLIGHT_REVALIDATIONS: u32 = 3;
const DELAY_TEST_CONCURRENCY: usize = 4;
const DELAY_SWEEP_COOLDOWN: Duration = Duration::from_secs(300);
const HEALTH_PROBE_INTERVAL: Duration = Duration::from_secs(10);
const HEALTH_ACTIVE_WINDOW: Duration = Duration::from_secs(120);
const HEALTH_IDLE_PROBE_INTERVAL: Duration = Duration::from_secs(120);
const HEALTH_FAST_PROBE_GAP: Duration = Duration::from_secs(1);
const HEALTH_PROBE_STALE: Duration = Duration::from_secs(5);
const PROXIES_STALE: Duration = Duration::from_secs(10);
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
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
const REFRESH_APPLY_MAX_DEFER: Duration = Duration::from_secs(2 * 60 * 60);
const REFRESH_DEGRADED_WINDOW: Duration = Duration::from_secs(5 * 60);
const PROXY_IP_STARTUP_GRACE: Duration = Duration::from_secs(30);
const SUBSCRIPTION_STARTUP_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const SUBSCRIPTION_REFRESH_MIN_BACKOFF: Duration = Duration::from_secs(5 * 60);
const SUBSCRIPTION_REFRESH_MAX_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);
const IDLE_METRICS_INTERVAL: Duration = Duration::from_secs(3);
const PROXIES_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const EVENT_QUEUE_WARN_DEPTH: usize = 64;
const EVENT_QUEUE_LOG_INTERVAL: Duration = Duration::from_secs(300);
pub const MAX_LIST_ROWS: usize = 200;
pub const TOAST_TTL: Duration = Duration::from_secs(3);
pub const MAX_CLIENTS: usize = 16;
const MAX_HEALTH_CONNECTION_IDS: usize = 8192;

fn connection_id_hash(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

fn daemon_exit_result(panic_message: Option<&str>) -> io::Result<()> {
    if let Some(message) = panic_message {
        Err(io::Error::other(format!(
            "daemon event loop panicked: {message}"
        )))
    } else {
        Ok(())
    }
}

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
    event_queue_peak: usize,
    events_processed_window: u64,
    last_event_queue_log: Option<Instant>,
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
pub enum HealthOutcome {
    Healthy(u64),
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryTarget {
    Auto,
    Node(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionNotice {
    Node,
    Auto,
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

async fn select_with_generation(
    client: ClashClient,
    target: &str,
    token: &Arc<AtomicU64>,
    generation: u64,
    retries: u32,
) -> Option<Result<(), ClashError>> {
    for attempt in 0..=retries {
        if token.load(Ordering::Relaxed) != generation {
            return None;
        }
        match client.select(target).await {
            Ok(()) => {
                return (token.load(Ordering::Relaxed) == generation).then_some(Ok(()));
            }
            Err(_) if attempt < retries => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(err) => {
                return (token.load(Ordering::Relaxed) == generation).then_some(Err(err));
            }
        }
    }
    None
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
    PreflightDone {
        plan: Box<StartPlan>,
        result: Result<(), String>,
    },
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
    ProxyStarted {
        epoch: u64,
        plan: Box<StartPlan>,
        result: Result<RunningProxy, String>,
    },
    ProxyStopped {
        epoch: u64,
    },
    Connections {
        epoch: u64,
        snapshot: ConnectionsSnapshot,
    },
    Proxies {
        epoch: u64,
        snapshot: ProxiesSnapshot,
    },
    ProxyIps {
        epoch: u64,
        ips: Vec<String>,
        errors: Vec<String>,
    },
    HealthProbe {
        generation: u64,
        tag: String,
        outcome: HealthOutcome,
    },
    RecoveryProbe {
        epoch: u64,
        generation: u64,
        trigger: String,
        healthy: Vec<(String, u64)>,
        had_node_failure: bool,
    },
    RecoveryApplied {
        epoch: u64,
        target: RecoveryTarget,
        trigger: String,
        generation: u64,
    },
    NodeDelay {
        epoch: u64,
        tag: String,
        delay: Option<u64>,
        failed: bool,
        clear_testing: bool,
    },
    NodeDelaySweepDone {
        epoch: u64,
    },
    NodeSelected(String),
    AutoSelected,
    SelectionApplied,
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
        tx: ipc::ClientTx,
        accept: tokio::sync::oneshot::Sender<bool>,
    },
    ClientDetached(u64),
    StartProxyRequest {
        client_id: u64,
    },
    AutoStart,
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
    tx: ipc::ClientTx,
}

pub struct App {
    pub config: Config,
    pub paths: Paths,
    pub logger: Logger,
    pub(crate) ui_state: UiState,
    pub(crate) proxy_runtime: ProxyRuntime,
    pub(crate) metrics_state: MetricsState,
    pub(crate) binary_state: BinaryState,
    pub(crate) selection_state: SelectionState,
    subscription_state: SubscriptionState,
    managed: bool,
    service: Option<ServiceIdentity>,
    pub should_quit: bool,
    health: HealthState,
    stats: RuntimeStats,
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
        binary_manager.recover_install();
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
            selection_state: SelectionState::default(),
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
        let connection_count = self.metrics_state.metrics.total_connections;
        let metrics = MetricsSnapshot {
            upload_total: self.metrics_state.metrics.upload_total,
            download_total: self.metrics_state.metrics.download_total,
            connections: if self.ui_state.page == Page::Connections {
                self.metrics_state.metrics.connections.clone()
            } else {
                Vec::new()
            },
            total_connections: connection_count,
            selection: self.metrics_state.metrics.selection.clone(),
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
                && !self.metrics_api_fresh(),
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
            subscription_apply_pending: self.subscription_state.pending_refresh_apply.as_ref().map(
                |pending| crate::view::SubscriptionApplyView {
                    added: pending.diff.added,
                    removed: pending.diff.removed,
                    auto_changed: pending.diff.auto_changed,
                },
            ),
            selected: self.ui_state.selected,
            preferred_node: self.selection_state.preferred_node.clone(),
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
        let _ = self.events_tx.send(AppEvent::AutoStart);
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        let now = Instant::now();
        let mut last_publish = now.checked_sub(PUBLISH_INTERVAL).unwrap_or(now);
        let loop_result = crate::guard::CatchUnwind::new(async {
            loop {
                let queued = self.events_rx.len();
                self.sample_event_queue(queued);
                let mut publish = false;
                tokio::select! {
                    maybe_event = self.events_rx.recv() => {
                        if let Some(event) = maybe_event {
                            self.stats.events_processed_window += 1;
                            self.handle_event(event).await;
                            for _ in 0..32 {
                                let Ok(event) = self.events_rx.try_recv() else {
                                    break;
                                };
                                self.stats.events_processed_window += 1;
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
        })
        .await;
        let panic_message = loop_result
            .err()
            .map(|payload| crate::guard::panic_message(payload.as_ref()));
        if let Some(message) = panic_message.as_deref() {
            self.logger.app(
                LogLevel::Error,
                format!("daemon event loop panicked: {message}"),
            );
        }
        self.broadcast(ServerMsg::Shutdown);
        self.clients.clear();
        if let Some(task) = self.proxy_runtime.metrics_task.take() {
            task.abort();
        }
        if let Some(mut task) = self.proxy_runtime.start_task.take() {
            if tokio::time::timeout(Duration::from_secs(25), &mut task)
                .await
                .is_err()
            {
                self.logger.app(
                    LogLevel::Error,
                    "proxy start task did not finish during shutdown; aborting",
                );
                task.abort();
                let _ = task.await;
            }
        }
        let mut completed_start = None;
        while let Ok(event) = self.events_rx.try_recv() {
            if let AppEvent::ProxyStarted {
                epoch,
                result: Ok(proxy),
                ..
            } = event
            {
                if epoch == self.proxy_runtime.epoch {
                    completed_start = Some(proxy);
                }
            }
        }
        if let Some(proxy) = self.proxy_runtime.proxy.take().or(completed_start) {
            let logger = self.logger.clone();
            if tokio::time::timeout(Duration::from_secs(6), supervisor::stop(proxy, &logger))
                .await
                .is_err()
            {
                self.logger.app(
                    LogLevel::Error,
                    "sing-box stop timed out during daemon shutdown",
                );
            }
        }
        if let Some(mut task) = self.proxy_runtime.stop_task.take() {
            if tokio::time::timeout(Duration::from_secs(10), &mut task)
                .await
                .is_err()
            {
                self.logger.app(
                    LogLevel::Error,
                    "proxy stop task did not finish during shutdown; aborting",
                );
                task.abort();
                let _ = task.await;
            }
        }
        let _ = std::fs::remove_file(&socket_path);
        daemon_exit_result(panic_message.as_deref())
    }

    async fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => {
                self.handle_key(key).await;
                self.remember_connection_cursor();
            }
            AppEvent::BackgroundPanic { task, message } => {
                self.on_background_panic(task, message);
            }
            AppEvent::PreflightDone { plan, result } => {
                self.finish_restart_after_preflight(*plan, result);
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
                let node_count: usize = ordered.iter().map(|item| item.nodes.len()).sum();
                let capped = node_count > subscription::MAX_TOTAL_NODES;
                self.subscription_state.subscriptions = ordered;
                self.recompute_nodes();
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
                let name = runtime.config.name.clone();
                let error = runtime.error.clone();
                let node_count = runtime.nodes.len();
                if let Some(existing) = self
                    .subscription_state
                    .subscriptions
                    .iter_mut()
                    .find(|item| item.config.id == id)
                {
                    *existing = runtime;
                } else {
                    self.subscription_state.subscriptions.push(runtime);
                }
                self.recompute_nodes();
                self.finalize_subscription_change();
                match error {
                    Some(err) => {
                        self.logger.app(
                            LogLevel::Warn,
                            format!("subscription {name} refresh failed: {err}"),
                        );
                        self.schedule_subscription_retry();
                    }
                    None => {
                        self.logger.app(
                            LogLevel::Info,
                            format!("subscription {name} refreshed: {node_count} nodes"),
                        );
                        self.clear_refresh_retry_if_healthy();
                    }
                }
                self.after_subscription_update(&id);
            }
            AppEvent::DownloadDone(result) => {
                self.binary_state.setup.downloading = false;
                match result {
                    Ok(metadata) => {
                        self.binary_state.latest_binary_version = Some(metadata.version.clone());
                        self.binary_state.last_update_check = Some(Instant::now());
                        self.apply_binary_metadata(&metadata);
                        self.notify(format!(
                            "{} {}",
                            self.text().msg_binary_installed,
                            metadata.version
                        ));
                        self.close_overlay();
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
                        self.binary_state.latest_binary_version = None;
                        self.binary_state.last_update_check = None;
                        self.binary_state.setup.pending_install = None;
                        self.apply_binary_metadata(&metadata);
                        self.notify(format!(
                            "{} {}",
                            self.text().msg_binary_installed,
                            metadata.version
                        ));
                        self.close_overlay();
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
            AppEvent::ProxyStarted {
                epoch,
                plan,
                result,
            } => {
                if epoch != self.proxy_runtime.epoch {
                    return;
                }
                self.proxy_runtime.start_task = None;
                self.proxy_runtime.preflight_revalidations = 0;
                self.proxy_runtime.preflight_force = false;
                match result {
                    Ok(proxy) => {
                        let clash = proxy.clash.clone();
                        let ipv6 = proxy.ipv6;
                        self.proxy_runtime.proxy = Some(proxy);
                        self.proxy_runtime.status = ProxyStatus::Running;
                        self.proxy_runtime.restart_in_progress = false;
                        self.selection_state.pending_auto_start = false;
                        self.proxy_runtime.started_at = Some(Instant::now());
                        self.proxy_runtime.restart_at = None;
                        self.proxy_runtime.stable_reset_at =
                            Some(Instant::now() + Duration::from_secs(60));
                        self.proxy_runtime.last_running_plan = Some(*plan);
                        self.proxy_runtime.pending_restart_plan = None;
                        self.proxy_runtime.restart_fallback_plan = None;
                        self.metrics_state.last_totals = None;
                        self.metrics_state.up_speed = 0.0;
                        self.metrics_state.down_speed = 0.0;
                        self.ui_state.last_error = None;
                        self.reset_health_state();
                        self.metrics_state.proxy_ip_backoff_secs = 5;
                        self.metrics_state.proxy_ip_failures = 0;
                        self.start_metrics(epoch, clash.port, clash.secret.clone());
                        self.apply_pending_selection();
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
                        self.logger
                            .app(LogLevel::Error, format!("sing-box start failed: {err}"));
                        let fallback = self.proxy_runtime.restart_fallback_plan.take();
                        if let Some(fallback) =
                            fallback.filter(|fallback| fallback != plan.as_ref())
                        {
                            self.proxy_runtime.status = ProxyStatus::Failed;
                            self.proxy_runtime.restart_in_progress = true;
                            self.logger.app(
                                LogLevel::Warn,
                                "new configuration failed to start; restoring the previous runtime plan",
                            );
                            match self.start_proxy_with_plan(fallback, None) {
                                Ok(()) => return,
                                Err(fallback_err) => {
                                    self.logger.app(
                                        LogLevel::Error,
                                        format!(
                                            "previous runtime plan also failed: {fallback_err}"
                                        ),
                                    );
                                }
                            }
                        }
                        self.proxy_runtime.status = ProxyStatus::Failed;
                        self.proxy_runtime.restart_in_progress = false;
                        self.proxy_runtime.runtime_has_auto = false;
                        self.selection_state.pending_auto_start = false;
                        self.proxy_runtime.proxy_node_tags.clear();
                        self.proxy_runtime.proxy_auto_tags.clear();
                        self.proxy_runtime.proxy_node_names.clear();
                        self.proxy_runtime.stable_reset_at = None;
                        self.ui_state.last_error = Some(err);
                        let detail = self.ui_state.last_error.clone().unwrap_or_default();
                        self.ack_pending_start(false, format!("proxy start failed: {detail}"));
                        self.schedule_restart();
                    }
                }
            }
            AppEvent::ProxyStopped { epoch } => {
                if epoch != self.proxy_runtime.epoch {
                    return;
                }
                self.proxy_runtime.stop_task = None;
                self.proxy_runtime.preflight_revalidations = 0;
                self.proxy_runtime.preflight_force = false;
                self.proxy_runtime.status = ProxyStatus::Stopped;
                self.proxy_runtime.runtime_has_auto = false;
                self.selection_state.pending_auto_start = false;
                self.proxy_runtime.proxy_node_tags.clear();
                self.proxy_runtime.proxy_auto_tags.clear();
                self.proxy_runtime.proxy_node_names.clear();
                self.proxy_runtime.started_at = None;
                self.proxy_runtime.restart_attempts = 0;
                self.proxy_runtime.restart_at = None;
                self.proxy_runtime.stable_reset_at = None;
                self.ui_state.restart_confirm = false;
                self.reset_metrics();
                let restart_plan = if self.proxy_runtime.restart_after_stop {
                    self.proxy_runtime.pending_restart_plan.take()
                } else {
                    self.proxy_runtime.pending_restart_plan = None;
                    None
                };
                self.proxy_runtime.restart_after_stop = false;
                if let Some(plan) = restart_plan {
                    let fallback = self
                        .proxy_runtime
                        .last_running_plan
                        .clone()
                        .filter(|fallback| fallback != &plan);
                    if let Err(err) = self.start_proxy_with_plan(plan, fallback.clone()) {
                        self.logger.app(
                            LogLevel::Error,
                            format!("restart plan failed validation: {err}"),
                        );
                        if let Some(fallback) = fallback {
                            if self.start_proxy_with_plan(fallback, None).is_ok() {
                                return;
                            }
                        }
                        self.proxy_runtime.restart_in_progress = false;
                        self.ui_state.last_error = Some(err);
                        self.schedule_restart();
                    }
                } else {
                    self.proxy_runtime.restart_in_progress = false;
                }
            }
            AppEvent::Connections { epoch, snapshot } => {
                if epoch != self.proxy_runtime.epoch {
                    return;
                }
                let now = Instant::now();
                if let Some((up, down, at)) = self.metrics_state.last_totals {
                    let seconds = now.duration_since(at).as_secs_f64();
                    if seconds >= 0.2 {
                        self.metrics_state.up_speed =
                            snapshot.upload_total.saturating_sub(up) as f64 / seconds;
                        self.metrics_state.down_speed =
                            snapshot.download_total.saturating_sub(down) as f64 / seconds;
                    }
                }
                self.metrics_state.last_totals =
                    Some((snapshot.upload_total, snapshot.download_total, now));
                self.health.last_connections_at = Some(now);
                let connection_ids = snapshot
                    .connections
                    .iter()
                    .take(MAX_HEALTH_CONNECTION_IDS)
                    .map(|connection| connection_id_hash(&connection.id));
                let new_connection = connection_ids
                    .clone()
                    .any(|id| !self.health.connection_ids.contains(&id));
                self.health.connection_ids.clear();
                self.health.connection_ids.extend(connection_ids);
                // Probes and proxy-IP lookups create their own connections through the
                // proxy; ignoring them while in flight avoids a self-triggered probe loop.
                if new_connection
                    && !self.metrics_state.proxy_ips_in_flight
                    && !self.health.probe_in_flight
                {
                    self.health.last_activity = Some(now);
                    self.health.fast_probe_pending = true;
                }
                self.metrics_state.metrics.upload_total = snapshot.upload_total;
                self.metrics_state.metrics.download_total = snapshot.download_total;
                self.metrics_state.metrics.connections = snapshot.connections;
                self.metrics_state.metrics.total_connections = snapshot.total_connections;
                self.reindex_connection_selection();
                if snapshot.total_connections == 0 {
                    self.metrics_state.idle_since.get_or_insert(now);
                } else {
                    self.metrics_state.idle_since = None;
                }
            }
            AppEvent::Proxies { epoch, snapshot } => {
                if epoch != self.proxy_runtime.epoch {
                    return;
                }
                let now = Instant::now();
                self.health.last_proxies_at = Some(now);
                self.metrics_state.metrics.selection = snapshot.selection;
                for (tag, delay) in snapshot.node_delays {
                    self.metrics_state.node_delays.insert(tag, delay);
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
                        self.health.quality.record_success(&tag, delay);
                        self.metrics_state.node_delays.insert(tag.clone(), delay);
                        if self.health_target().as_deref() != Some(tag.as_str()) {
                            self.sort_nodes_if_visible();
                            return;
                        }
                        let recovered = self.health.pause_until.take().is_some()
                            || self.health.no_node_level > 0;
                        let had_failures = self.health.failures > 0;
                        self.health.no_node_level = 0;
                        self.health.failures = 0;
                        self.health.failure_tag = None;
                        self.clear_health_error();
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
                        self.sort_nodes_if_visible();
                    }
                    HealthOutcome::Failed => {
                        if self.health_target().as_deref() != Some(tag.as_str()) {
                            self.reset_health_failures();
                            return;
                        }
                        self.stats.health_probe_failures_total =
                            self.stats.health_probe_failures_total.saturating_add(1);
                        self.health.last_failure_at = Some(Instant::now());
                        self.health.last_failure_tag = Some(tag.clone());
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
                epoch,
                generation,
                trigger,
                healthy,
                had_node_failure,
            } => {
                if epoch != self.proxy_runtime.epoch || generation != self.health.generation {
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
                    self.recovery_found_no_target(had_node_failure);
                    return;
                };
                let Some(proxy) = self.proxy_runtime.proxy.as_ref() else {
                    self.stats.recovery_aborted = self.stats.recovery_aborted.saturating_add(1);
                    return;
                };
                let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
                let tag = match &target {
                    RecoveryTarget::Auto => tags::AUTO.to_string(),
                    RecoveryTarget::Node(tag) => tag.clone(),
                };
                self.selection_state
                    .selection_generation
                    .fetch_add(1, Ordering::Relaxed);
                let tx = self.events_tx.clone();
                let generation = self
                    .selection_state
                    .selection_generation
                    .load(Ordering::Relaxed);
                let token = self.selection_state.selection_generation.clone();
                self.spawn_guarded(BackgroundTask::Selection, async move {
                    match select_with_generation(client, &tag, &token, generation, 0).await {
                        Some(Ok(())) => {
                            let _ = tx.send(AppEvent::RecoveryApplied {
                                epoch,
                                target,
                                trigger,
                                generation,
                            });
                        }
                        Some(Err(err)) => {
                            let _ = tx.send(AppEvent::Error(err.to_string()));
                        }
                        None => {}
                    }
                });
            }
            AppEvent::RecoveryApplied {
                epoch,
                target,
                trigger,
                generation,
            } => {
                if epoch != self.proxy_runtime.epoch
                    || generation
                        != self
                            .selection_state
                            .selection_generation
                            .load(Ordering::Relaxed)
                {
                    return;
                }
                if self.proxy_runtime.status != ProxyStatus::Running
                    || self.proxy_runtime.proxy.is_none()
                    || self.health_target().as_deref() != Some(trigger.as_str())
                {
                    return;
                }
                let (tag, name) = match &target {
                    RecoveryTarget::Auto => {
                        self.selection_state.preferred_node = None;
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
                        self.selection_state.preferred_node = Some(tag.clone());
                        (tag.clone(), name)
                    }
                };
                self.selection_state.pending_selection = None;
                self.stats.recovery_selected = self.stats.recovery_selected.saturating_add(1);
                self.set_switch_reason("recovery");
                self.health.failures = 0;
                self.health.failure_tag = None;
                self.health.no_node_level = 0;
                self.health.pause_until = None;
                self.clear_health_error();
                self.record_auto_switch();
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
                self.notify(label);
                if self.enforce_switch_budget_after_record() {
                    self.logger.app(
                        LogLevel::Warn,
                        "health failover budget reached; automatic recovery paused",
                    );
                }
                self.request_metrics_refresh();
            }
            AppEvent::ProxyIps { epoch, ips, errors } => {
                if epoch != self.proxy_runtime.epoch {
                    return;
                }
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
            AppEvent::NodeDelay {
                epoch,
                tag,
                delay,
                failed,
                clear_testing,
            } => {
                if epoch != self.proxy_runtime.epoch {
                    return;
                }
                if clear_testing {
                    self.metrics_state.delay_testing.remove(&tag);
                }
                if self.proxy_runtime.proxy.is_none() {
                    return;
                }
                self.record_delay_measurement(tag, delay, failed);
            }
            AppEvent::NodeSelected(tag) => {
                if self.metrics_state.nodes.iter().any(|node| node.tag == tag) {
                    if self.metrics_state.metrics.selection.selected.as_deref()
                        != Some(tag.as_str())
                    {
                        self.set_switch_reason("manual");
                    }
                    self.selection_state.preferred_node = Some(tag);
                }
            }
            AppEvent::AutoSelected => {
                self.selection_state.preferred_node = None;
                self.selection_state.pending_selection = None;
                self.set_switch_reason("manual");
            }
            AppEvent::SelectionApplied => self.request_metrics_refresh(),
            AppEvent::NodeDelaySweepDone { epoch } => {
                if epoch != self.proxy_runtime.epoch {
                    return;
                }
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
                        self.apply_binary_metadata(&metadata);
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
                    if accept.send(true).is_err() {
                        self.clients.remove(&id);
                        return;
                    }
                    if viewer {
                        self.metrics_state
                            .viewer_active
                            .store(true, Ordering::Relaxed);
                        self.metrics_state.metrics_wake.notify_one();
                        if self.ui_state.page == Page::Logs && self.ui_state.log_follow {
                            self.ui_state.selected = self.log_count().saturating_sub(1);
                        }
                        self.sort_nodes_if_visible();
                        self.remember_connection_cursor();
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
                self.handle_start_request(Some(client_id));
            }
            AppEvent::AutoStart => {
                self.handle_start_request(None);
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
        if let Some(client_id) = self.selection_state.pending_start.take() {
            self.ack(client_id, ok, message);
        }
    }

    fn handle_start_request(&mut self, client_id: Option<u64>) {
        if let Some(client_id) = client_id {
            if !self.clients.contains_key(&client_id) {
                return;
            }
        }
        if self.proxy_runtime.status == ProxyStatus::Running {
            if let Some(client_id) = client_id {
                self.ack(client_id, true, "proxy is already running");
            } else {
                self.selection_state.pending_auto_start = false;
            }
            return;
        }
        if self.proxy_busy() {
            if let Some(client_id) = client_id {
                self.ack(client_id, false, "proxy state is changing");
            }
            return;
        }
        if self.setup_busy() {
            if let Some(client_id) = client_id {
                self.ack(client_id, false, "binary setup is in progress");
            }
            return;
        }
        if self.metrics_state.nodes.is_empty() {
            if let Some(client_id) = client_id {
                self.selection_state.pending_start = Some(client_id);
            } else {
                self.selection_state.pending_auto_start = true;
            }
            if self.subscription_state.refreshing_subscriptions {
                return;
            }
            if self.config.enabled_subscriptions().next().is_some() {
                self.refresh_subscriptions();
                return;
            }
            self.ack_pending_start(false, "no nodes available");
            self.selection_state.pending_auto_start = false;
            if client_id.is_none() {
                self.ui_state.last_error = Some(self.text().no_nodes.to_string());
                self.logger
                    .app(LogLevel::Warn, "auto-start: no nodes available");
            }
            return;
        }
        match self.start_proxy() {
            Ok(()) => {
                self.selection_state.pending_auto_start = false;
                if let Some(client_id) = client_id {
                    self.selection_state.pending_start = Some(client_id);
                } else {
                    self.logger
                        .app(LogLevel::Info, "auto-start: starting proxy");
                }
            }
            Err(err) => {
                self.selection_state.pending_auto_start = false;
                if let Some(client_id) = client_id {
                    self.ack(client_id, false, err);
                } else {
                    self.logger
                        .app(LogLevel::Warn, format!("auto-start: {err}"));
                }
            }
        }
    }

    fn retry_pending_start(&mut self) {
        let has_client = self.selection_state.pending_start.is_some();
        let has_auto = self.selection_state.pending_auto_start;
        if !has_client && !has_auto {
            return;
        }
        if self.proxy_runtime.status == ProxyStatus::Running || self.proxy_runtime.proxy.is_some() {
            self.selection_state.pending_auto_start = false;
            self.ack_pending_start(true, "proxy is already running");
            return;
        }
        if self.metrics_state.nodes.is_empty() {
            if self.subscription_state.refreshing_subscriptions {
                return;
            }
            self.ack_pending_start(false, "no nodes available");
            self.selection_state.pending_auto_start = false;
            if has_auto && self.ui_state.last_error.is_none() {
                self.ui_state.last_error = Some(self.text().no_nodes.to_string());
                self.logger
                    .app(LogLevel::Warn, "auto-start: no nodes available");
            }
            return;
        }
        if self.proxy_busy() {
            return;
        }
        if self.setup_busy() {
            return;
        }
        match self.start_proxy() {
            Ok(()) => {
                self.selection_state.pending_auto_start = false;
            }
            Err(err) => {
                self.selection_state.pending_auto_start = false;
                if has_client {
                    self.ack_pending_start(false, err);
                } else {
                    self.logger
                        .app(LogLevel::Warn, format!("auto-start: {err}"));
                }
            }
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
        let view = Arc::new(self.view());
        for client in self.clients.values().filter(|client| client.viewer) {
            client.tx.send_view(view.clone());
        }
    }

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
                self.proxy_runtime.start_task = None;
                self.proxy_runtime.restart_in_progress = false;
                self.proxy_runtime.status = ProxyStatus::Failed;
                self.selection_state.pending_auto_start = false;
                self.ack_pending_start(false, "proxy start task failed");
                self.schedule_restart();
            }
            BackgroundTask::ProxyStop => {
                self.proxy_runtime.stop_task = None;
                self.proxy_runtime.restart_in_progress = false;
                self.proxy_runtime.status = ProxyStatus::Stopped;
                self.proxy_runtime.restart_after_stop = false;
                self.proxy_runtime.pending_restart_plan = None;
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

    fn sample_event_queue(&mut self, current: usize) {
        self.stats.event_queue_peak = self.stats.event_queue_peak.max(current);
        let due = self
            .stats
            .last_event_queue_log
            .is_none_or(|last| last.elapsed() >= EVENT_QUEUE_LOG_INTERVAL);
        if !due {
            return;
        }
        if self.stats.event_queue_peak >= EVENT_QUEUE_WARN_DEPTH {
            self.logger.app(
                LogLevel::Info,
                format!(
                    "event queue peak {} (current {}), {} events processed in window",
                    self.stats.event_queue_peak, current, self.stats.events_processed_window
                ),
            );
        }
        self.stats.event_queue_peak = 0;
        self.stats.events_processed_window = 0;
        self.stats.last_event_queue_log = Some(Instant::now());
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
        if let Some(tag) = self.selection_state.preferred_node.clone() {
            if !self.metrics_state.nodes.iter().any(|node| node.tag == tag) {
                let name = self
                    .proxy_runtime
                    .proxy_node_names
                    .get(&tag)
                    .map(|name| sanitize_owned(name.clone()))
                    .filter(|name| !name.is_empty())
                    .map(|name| format!(" ({name})"))
                    .unwrap_or_default();
                self.selection_state.preferred_node = None;
                self.selection_state.pending_selection = None;
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
        if self.setup_busy() {
            self.notify(self.text().setup_busy);
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

    fn start_metrics(&mut self, epoch: u64, port: u16, secret: String) {
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
                let mut next_proxies = Instant::now();
                let mut last_connection_error: Option<String> = None;
                let mut last_proxy_error: Option<String> = None;
                loop {
                    let mut interval = Duration::from_secs(1);
                    match client.connections().await {
                        Ok(snapshot) => {
                            last_connection_error = None;
                            if snapshot.total_connections == 0
                                && !viewer_active.load(Ordering::Relaxed)
                            {
                                interval = IDLE_METRICS_INTERVAL;
                            }
                            if tx.send(AppEvent::Connections { epoch, snapshot }).is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            report_clash_error(
                                &tx,
                                clash_api_label,
                                &mut last_connection_error,
                                err,
                            );
                        }
                    }
                    if Instant::now() >= next_proxies
                        || proxies_refresh.swap(false, Ordering::Relaxed)
                    {
                        match client.proxies().await {
                            Ok(snapshot) => {
                                last_proxy_error = None;
                                if tx.send(AppEvent::Proxies { epoch, snapshot }).is_err() {
                                    break;
                                }
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
        let epoch = self.proxy_runtime.epoch;
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
            let _ = tx.send(AppEvent::ProxyIps { epoch, ips, errors });
        });
    }

    fn spawn_selection_put(
        &mut self,
        target: String,
        label: String,
        retry: bool,
        notice: Option<SelectionNotice>,
    ) {
        let Some(proxy) = self.proxy_runtime.proxy.as_ref() else {
            return;
        };
        let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
        let generation = self
            .selection_state
            .selection_generation
            .load(Ordering::Relaxed);
        let token = self.selection_state.selection_generation.clone();
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::Selection, async move {
            let retries = if retry { 19 } else { 0 };
            match select_with_generation(client, &target, &token, generation, retries).await {
                Some(Ok(())) => {
                    match notice {
                        Some(SelectionNotice::Node) => {
                            let _ = tx.send(AppEvent::NodeSelected(target));
                        }
                        Some(SelectionNotice::Auto) => {
                            let _ = tx.send(AppEvent::AutoSelected);
                        }
                        None => {}
                    }
                    let _ = tx.send(AppEvent::Info(label));
                    let _ = tx.send(AppEvent::SelectionApplied);
                }
                Some(Err(err)) => {
                    let _ = tx.send(AppEvent::Error(err.to_string()));
                }
                None => {}
            }
        });
    }

    fn apply_binary_metadata(&mut self, metadata: &BinaryMetadata) {
        self.binary_state.binary_version = Some(metadata.version.clone());
        self.binary_state.binary_verified = Some(metadata.verified);
        self.binary_state.binary_installed = true;
        self.binary_state.setup.message = None;
        self.refresh_previous_binary_state();
    }

    fn select_node(&mut self) {
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
            self.selection_state.preferred_node = Some(node.tag.clone());
            self.selection_state.pending_selection = Some(PendingSelection::Node(node.tag.clone()));
            self.notify(format!(
                "{}: {}",
                self.text().msg_node_selected_pending,
                node.name
            ));
            return;
        }
        self.selection_state
            .selection_generation
            .fetch_add(1, Ordering::Relaxed);
        if !self.proxy_runtime.proxy_node_tags.is_empty()
            && !self.proxy_runtime.proxy_node_tags.contains(&node.tag)
        {
            self.selection_state.preferred_node = Some(node.tag.clone());
            self.selection_state.pending_selection = None;
            let restarting = self.auto_restart_after_change();
            self.notify(if restarting {
                self.text().msg_node_switching
            } else {
                self.text().msg_node_not_in_runtime
            });
            return;
        }
        let tag = node.tag.clone();
        let name = node.name.clone();
        self.selection_state.pending_selection = None;
        self.invalidate_health_tasks();
        self.request_metrics_refresh();
        self.notify(format!("{}: {name}", self.text().msg_node_selected));
        let label = format!("{}: {name}", self.text().msg_node_selected);
        self.spawn_selection_put(tag, label, false, Some(SelectionNotice::Node));
    }

    fn apply_pending_selection(&mut self) {
        let Some(pending) = self.selection_state.pending_selection.take() else {
            return;
        };
        let (target, label, notice) = match pending {
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
                (
                    target,
                    self.text().msg_auto_enabled.to_string(),
                    SelectionNotice::Auto,
                )
            }
            PendingSelection::Node(tag) => {
                let name = self
                    .metrics_state
                    .nodes
                    .iter()
                    .find(|node| node.tag == tag)
                    .map(|node| node.name.clone())
                    .unwrap_or_else(|| tag.clone());
                (
                    tag,
                    format!("{}: {name}", self.text().msg_node_selected),
                    SelectionNotice::Node,
                )
            }
        };
        self.request_metrics_refresh();
        self.spawn_selection_put(target, label, true, Some(notice));
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
        if !self.metrics_state.delay_testing.insert(tag.clone()) {
            return;
        }
        let epoch = self.proxy_runtime.epoch;
        let test_url = self.config.test_url.clone();
        let label = self.text().msg_node_delay;
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::DelayTest, async move {
            let result = client.delay(&tag, &test_url).await;
            let failed = matches!(health_outcome(&result), HealthOutcome::Failed);
            match result {
                Ok(delay) => {
                    let _ = tx.send(AppEvent::NodeDelay {
                        epoch,
                        tag: tag.clone(),
                        delay: Some(delay),
                        failed: false,
                        clear_testing: true,
                    });
                    let _ = tx.send(AppEvent::Info(format!("{label}: {tag} {delay} ms")));
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::NodeDelay {
                        epoch,
                        tag,
                        delay: None,
                        failed,
                        clear_testing: true,
                    });
                    let _ = tx.send(AppEvent::Error(err.to_string()));
                }
            }
        });
    }

    fn maybe_test_node_delays(&mut self, force: bool) {
        if self.proxy_runtime.proxy.is_none()
            || self.metrics_state.delay_sweep_running
            || !self.metrics_state.delay_testing.is_empty()
        {
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
        let epoch = self.proxy_runtime.epoch;
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
                        let result = client.delay(&tag, &test_url).await;
                        let failed = matches!(health_outcome(&result), HealthOutcome::Failed);
                        (tag, result.ok(), failed)
                    });
                }
                while let Some(result) = tasks.join_next().await {
                    if let Ok((tag, delay, failed)) = result {
                        let _ = tx.send(AppEvent::NodeDelay {
                            epoch,
                            tag,
                            delay,
                            failed,
                            clear_testing: true,
                        });
                    }
                }
            }
            let _ = tx.send(AppEvent::NodeDelaySweepDone { epoch });
        });
    }

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
            self.selection_state.preferred_node = None;
            self.selection_state.pending_selection = Some(PendingSelection::Auto);
            self.notify(self.text().msg_auto_pending);
            return;
        }
        self.selection_state
            .selection_generation
            .fetch_add(1, Ordering::Relaxed);
        let Some(target) = self.auto_selection_target() else {
            self.ui_state.last_error = Some(self.text().no_matching_nodes.to_string());
            return;
        };
        if target == tags::AUTO && !self.proxy_runtime.runtime_has_auto {
            self.selection_state.preferred_node = None;
            self.selection_state.pending_selection = Some(PendingSelection::Auto);
            if self.auto_restart_after_change() {
                self.notify(self.text().msg_auto_restarting);
            } else {
                self.selection_state.pending_selection = None;
            }
            return;
        }
        if self.proxy_runtime.proxy.is_none() {
            return;
        }
        self.selection_state.pending_selection = None;
        self.invalidate_health_tasks();
        self.request_metrics_refresh();
        let label = self.text().msg_auto_enabled.to_string();
        self.spawn_selection_put(target, label, false, Some(SelectionNotice::Auto));
    }

    fn record_delay_measurement(&mut self, tag: String, delay: Option<u64>, failed: bool) {
        if let Some(delay) = delay.filter(|delay| *delay > 0) {
            self.health.quality.record_success(&tag, delay);
            self.metrics_state.node_delays.insert(tag, delay);
            self.sort_nodes_if_visible();
        } else if failed {
            self.health.quality.record_failure(&tag);
        }
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
        self.metrics_state.connection_cursor = None;
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
            match proxy.child.try_wait() {
                Ok(Some(status)) => crashed = Some(status.to_string()),
                Ok(None) => {}
                Err(err) => {
                    self.logger
                        .app(LogLevel::Warn, format!("sing-box wait failed: {err}"));
                    crashed = Some(format!("wait error: {err}"));
                }
            }
        }
        if let Some(status) = crashed {
            self.proxy_runtime.proxy = None;
            if let Some(task) = self.proxy_runtime.metrics_task.take() {
                task.abort();
            }
            self.proxy_runtime.status = ProxyStatus::Failed;
            self.proxy_runtime.runtime_has_auto = false;
            self.selection_state.pending_auto_start = false;
            self.proxy_runtime.stable_reset_at = None;
            self.proxy_runtime.proxy_node_tags.clear();
            self.proxy_runtime.proxy_auto_tags.clear();
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
            && !self.setup_busy()
            && self
                .proxy_runtime
                .restart_at
                .is_some_and(|at| Instant::now() >= at)
        {
            self.proxy_runtime.restart_at = None;
            if self.start_proxy().is_err() {
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
        let fallback = self.selection_state.preferred_node.clone();
        match active.or(fallback) {
            Some(tag) if tag == tags::AUTO => Some(self.text().common_auto.to_string()),
            Some(tag) => Some(self.node_display_name(&tag)),
            None if !self.metrics_state.nodes.is_empty() => {
                Some(self.text().common_auto.to_string())
            }
            None => None,
        }
    }

    pub fn selection_mode_auto(&self) -> bool {
        match self.metrics_state.metrics.selection.selected.as_deref() {
            Some(selected) => selected == tags::AUTO,
            None => self.selection_state.preferred_node.is_none(),
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
    let now = jiff::ZonedDateTime::now().datetime();
    let mut oldest = 0i64;
    let mut count = 0usize;
    for runtime in runtimes.iter().filter(|runtime| runtime.config.enabled) {
        count += 1;
        let updated = runtime
            .last_updated
            .as_deref()
            .and_then(|value| jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M:%S", value).ok())?;
        oldest = oldest.max(now.duration_since(updated).as_secs_f64().max(0.0) as i64);
    }
    (count > 0).then(|| Duration::from_secs(oldest as u64))
}

fn sort_nodes(
    nodes: &mut [Node],
    mode: NodeSort,
    auto_tags: &HashSet<String>,
    delays: &HashMap<String, u64>,
) {
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
