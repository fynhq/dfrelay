use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};

use crate::config::{Config, Paths, SubscriptionConfig};
use crate::i18n::Text;
use crate::ipc::{self, ServerMsg};
use crate::logging::{LogLevel, LogLine, LogSource, LogTimestamp, Logger};
use crate::netinfo;
use crate::service::ServiceIdentity;
use crate::singbox::binary::{is_newer_version, BinaryManager, BinaryMetadata, InboxCandidate};
use crate::singbox::clash::{ClashClient, ClashError, MetricsSnapshot, ProxySelection};
use crate::singbox::supervisor::{self, RunningProxy};
use crate::subscription::model::Node;
use crate::subscription::{self, merge_nodes, FetchOptions, SubscriptionRuntime};
use crate::text::sanitize_display;
use crate::view::{BinaryStatus, DaemonStatus, View};

const MAX_RESTART_ATTEMPTS: u32 = 5;
const RESTART_DELAYS: [u64; MAX_RESTART_ATTEMPTS as usize] = [1, 2, 5, 10, 30];
const DELAY_TEST_CONCURRENCY: usize = 4;
const DELAY_SWEEP_COOLDOWN: Duration = Duration::from_secs(300);
const HEALTH_PROBE_INTERVAL: Duration = Duration::from_secs(10);
const HEALTH_ACTIVE_WINDOW: Duration = Duration::from_secs(120);
const HEALTH_PROBE_STALE: Duration = Duration::from_secs(5);
const HEALTH_FAILURE_THRESHOLD: u32 = 3;
const HEALTH_RECOVERY_COOLDOWN: Duration = Duration::from_secs(120);
const HEALTH_NO_NODE_PAUSE: Duration = Duration::from_secs(300);
const HEALTH_REFRESH_COOLDOWN: Duration = Duration::from_secs(600);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);
const IDLE_METRICS_INTERVAL: Duration = Duration::from_secs(3);
const PROXIES_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
pub const MAX_LIST_ROWS: usize = 200;
pub const TOAST_TTL: Duration = Duration::from_secs(3);
pub const VIEW_QUEUE_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(usize)]
pub enum Page {
    Dashboard,
    Nodes,
    Subscriptions,
    Connections,
    Logs,
    Settings,
    Setup,
    Help,
}

pub const MAIN_PAGES: [Page; 6] = [
    Page::Dashboard,
    Page::Nodes,
    Page::Subscriptions,
    Page::Connections,
    Page::Logs,
    Page::Settings,
];

impl Page {
    pub fn title(self, text: &Text) -> &'static str {
        match self {
            Self::Dashboard => text.nav_dashboard,
            Self::Nodes => text.nav_nodes,
            Self::Subscriptions => text.nav_subscriptions,
            Self::Connections => text.nav_connections,
            Self::Logs => text.nav_logs,
            Self::Settings => text.nav_settings,
            Self::Setup => text.nav_setup,
            Self::Help => text.nav_help,
        }
    }

    pub fn main_index(self) -> usize {
        MAIN_PAGES
            .iter()
            .position(|page| *page == self)
            .unwrap_or(0)
    }

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn next_main(self) -> Self {
        let index = self.main_index();
        MAIN_PAGES[(index + 1) % MAIN_PAGES.len()]
    }

    pub fn previous_main(self) -> Self {
        let index = self.main_index();
        MAIN_PAGES[(index + MAIN_PAGES.len() - 1) % MAIN_PAGES.len()]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyStatus {
    Stopped,
    Starting,
    Stopping,
    Running,
    Failed,
}

impl ProxyStatus {
    pub fn label(&self, text: &Text) -> &'static str {
        match self {
            Self::Stopped => text.status_stopped,
            Self::Starting => text.status_starting,
            Self::Stopping => text.status_stopping,
            Self::Running => text.status_running,
            Self::Failed => text.status_failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum NodeSort {
    #[default]
    AutoLatency,
    Name,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SetupState {
    pub candidates: Vec<InboxCandidate>,
    pub message: Option<String>,
    pub downloading: bool,
    pub installing: bool,
    pub rollback_in_flight: bool,
    pub pending_install: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LineInput {
    pub value: String,
    pub cursor: usize,
}

impl LineInput {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    fn byte_index(&self, cursor: usize) -> usize {
        self.value
            .char_indices()
            .nth(cursor)
            .map(|(index, _)| index)
            .unwrap_or(self.value.len())
    }

    pub fn insert(&mut self, ch: char) {
        let index = self.byte_index(self.cursor);
        self.value.insert(index, ch);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        let length = self.value.chars().count();
        if self.cursor >= length {
            return;
        }
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.value.replace_range(start..end, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.value.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.value.chars().count();
    }

    pub fn trim_start_end(&self) -> String {
        self.value.trim().to_string()
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionForm {
    pub editing_id: Option<String>,
    pub name: LineInput,
    pub url: LineInput,
    pub enabled: bool,
    pub focus: usize,
    pub error: Option<String>,
}

impl SubscriptionForm {
    const FOCUS_COUNT: usize = 3;

    pub fn new() -> Self {
        Self {
            editing_id: None,
            name: LineInput::default(),
            url: LineInput::default(),
            enabled: true,
            focus: 0,
            error: None,
        }
    }

    pub fn edit(config: &SubscriptionConfig) -> Self {
        Self {
            editing_id: Some(config.id.clone()),
            name: LineInput::new(config.name.clone()),
            url: LineInput::new(config.url.clone()),
            enabled: config.enabled,
            focus: 0,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingSelection {
    Auto,
    Node(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeywordField {
    Include,
    Exclude,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordForm {
    pub field: KeywordField,
    pub value: LineInput,
    pub error: Option<String>,
}

impl KeywordForm {
    pub fn from_config(config: &Config, field: KeywordField) -> Self {
        let keywords = match field {
            KeywordField::Include => &config.keywords_include,
            KeywordField::Exclude => &config.keywords_exclude,
        };
        Self {
            field,
            value: LineInput::new(keywords.join(", ")),
            error: None,
        }
    }

    pub fn title(&self, text: &Text) -> &'static str {
        match self.field {
            KeywordField::Include => text.settings_keywords_include,
            KeywordField::Exclude => text.settings_keywords_exclude,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsItem {
    Language,
    Port,
    AllowLan,
    LanAuth,
    DnsServers,
    TestUrl,
    KeywordsInclude,
    KeywordsExclude,
    AppLogLevel,
    BinaryInfo,
    ConfigPath,
    LogFile,
}

pub const SETTINGS_ITEMS: [SettingsItem; 12] = [
    SettingsItem::Language,
    SettingsItem::Port,
    SettingsItem::AllowLan,
    SettingsItem::LanAuth,
    SettingsItem::DnsServers,
    SettingsItem::TestUrl,
    SettingsItem::KeywordsInclude,
    SettingsItem::KeywordsExclude,
    SettingsItem::AppLogLevel,
    SettingsItem::BinaryInfo,
    SettingsItem::ConfigPath,
    SettingsItem::LogFile,
];

pub fn settings_items(allow_lan: bool) -> Vec<SettingsItem> {
    SETTINGS_ITEMS
        .iter()
        .copied()
        .filter(|item| allow_lan || *item != SettingsItem::LanAuth)
        .collect()
}

impl SettingsItem {
    pub fn read_only(self) -> bool {
        matches!(
            self,
            Self::Port | Self::DnsServers | Self::TestUrl | Self::ConfigPath | Self::LogFile
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingsField {
    LanUsername,
    LanPassword,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsForm {
    pub active: SettingsField,
    pub username: LineInput,
    pub password: LineInput,
    pub error: Option<String>,
}

impl SettingsForm {
    pub fn new(config: &Config) -> Self {
        Self {
            active: SettingsField::LanUsername,
            username: LineInput::new(config.lan_username.clone()),
            password: LineInput::new(config.lan_password.clone()),
            error: None,
        }
    }

    pub fn title(&self, text: &Text) -> &'static str {
        text.settings_lan_auth_title
    }

    pub fn hint(&self, text: &Text) -> &'static str {
        text.settings_lan_auth_hint
    }

    pub fn active_input_mut(&mut self) -> &mut LineInput {
        match self.active {
            SettingsField::LanUsername => &mut self.username,
            SettingsField::LanPassword => &mut self.password,
        }
    }

    pub fn switch_field(&mut self, forward: bool) {
        self.active = match (self.active, forward) {
            (SettingsField::LanUsername, true) => SettingsField::LanPassword,
            (SettingsField::LanPassword, false) => SettingsField::LanUsername,
            (field, _) => field,
        };
    }
}

/// Editable modal forms. Multi-field forms mark the active field with `▸`;
/// single-field forms rely on the cursor only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EditForm {
    Subscription(SubscriptionForm),
    Keyword(KeywordForm),
    Settings(SettingsForm),
}

impl EditForm {
    pub fn title(&self, text: &Text) -> &'static str {
        match self {
            Self::Subscription(form) => {
                if form.editing_id.is_some() {
                    text.subscription_form_edit_title
                } else {
                    text.subscription_form_add_title
                }
            }
            Self::Keyword(_) => text.settings_keywords_title,
            Self::Settings(form) => form.title(text),
        }
    }

    pub fn hint(&self, text: &Text) -> &'static str {
        match self {
            Self::Subscription(_) => text.subscription_form_hint,
            Self::Keyword(_) => text.settings_keywords_hint,
            Self::Settings(form) => form.hint(text),
        }
    }

    pub fn set_error(&mut self, error: Option<String>) {
        match self {
            Self::Subscription(form) => form.error = error,
            Self::Keyword(form) => form.error = error,
            Self::Settings(form) => form.error = error,
        }
    }
}

/// There is at most one overlay at a time. Edit forms and the read-only log
/// detail have separate interaction models.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Overlay {
    Edit(EditForm),
    LogDetail { line: LogLine, scroll: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayKind {
    Subscription,
    Keyword,
    Settings,
    LogDetail,
}

impl Overlay {
    fn kind(&self) -> OverlayKind {
        match self {
            Self::Edit(EditForm::Subscription(_)) => OverlayKind::Subscription,
            Self::Edit(EditForm::Keyword(_)) => OverlayKind::Keyword,
            Self::Edit(EditForm::Settings(_)) => OverlayKind::Settings,
            Self::LogDetail { .. } => OverlayKind::LogDetail,
        }
    }

    pub fn log_detail(&self) -> Option<(&LogLine, usize)> {
        match self {
            Self::LogDetail { line, scroll } => Some((line, *scroll)),
            Self::Edit(_) => None,
        }
    }
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
) -> Option<RecoveryTarget> {
    if healthy.is_empty() {
        return None;
    }
    if selected == Some("auto") {
        return Some(RecoveryTarget::Auto);
    }
    healthy
        .iter()
        .min_by_key(|(_, delay)| *delay)
        .map(|(tag, _)| RecoveryTarget::Node(tag.clone()))
}

pub enum AppEvent {
    Key(KeyEvent),
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
    ProxyIps(Vec<String>),
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
    pub page: Page,
    pub dashboard_scroll: usize,
    dashboard_max_scroll: usize,
    pub status: ProxyStatus,
    pub nodes: Vec<Node>,
    node_sort: NodeSort,
    pub subscriptions: Vec<SubscriptionRuntime>,
    pub selected: usize,
    page_selection: [usize; 8],
    pub metrics: MetricsSnapshot,
    pub node_delays: HashMap<String, u64>,
    pub delay_testing: HashSet<String>,
    pub up_speed: f64,
    pub down_speed: f64,
    last_totals: Option<(u64, u64, Instant)>,
    pub local_ips: Vec<String>,
    pub proxy_ips: Vec<String>,
    pub binary_version: Option<String>,
    pub binary_verified: Option<bool>,
    pub binary_installed: bool,
    pub latest_binary_version: Option<String>,
    pub previous_binary_available: bool,
    pub previous_binary_version: Option<String>,
    update_check_in_flight: bool,
    last_update_check: Option<Instant>,
    pub setup: SetupState,
    install_in_flight: bool,
    pending_restart_subscription: Option<String>,
    pub overlay: Option<Overlay>,
    log_detail_max_scroll: usize,
    pub subscription_delete: Option<String>,
    pub log_source: LogSource,
    pub log_filter: LogLevel,
    pub log_follow: bool,
    pub log_timestamp: LogTimestamp,
    pub last_error: Option<String>,
    pub message: Option<String>,
    pub message_at: Option<Instant>,
    pub restart_confirm: bool,
    pub rollback_confirm: bool,
    pub restart_in_progress: bool,
    restart_after_stop: bool,
    managed: bool,
    service: Option<ServiceIdentity>,
    pub should_quit: bool,
    pub help_scroll: usize,
    pub help_max_scroll: usize,
    pub last_local_ips: Instant,
    pub last_proxy_ips: Instant,
    proxy_ips_in_flight: bool,
    proxy_ip_backoff_secs: u64,
    delay_sweep_running: bool,
    last_delay_sweep: Option<Instant>,
    health_failures: u32,
    health_failure_tag: Option<String>,
    health_probe_in_flight: bool,
    health_generation: u64,
    last_health_probe: Option<Instant>,
    last_health_activity: Option<Instant>,
    health_connection_ids: HashSet<String>,
    last_metrics_at: Option<Instant>,
    recovery_in_flight: bool,
    last_recovery: Option<Instant>,
    health_pause_until: Option<Instant>,
    recovery_refresh_pending: bool,
    last_recovery_refresh: Option<Instant>,
    pub started_at: Option<Instant>,
    pub metrics_task: Option<JoinHandle<()>>,
    viewer_active: Arc<AtomicBool>,
    metrics_wake: Arc<Notify>,
    proxies_refresh: Arc<AtomicBool>,
    pub refreshing_subscriptions: bool,
    subscription_seq: u64,
    subscription_refresh_seq: HashMap<String, u64>,
    pub last_subscription_refresh: Option<Instant>,
    pub preferred_node: Option<String>,
    runtime_has_auto: bool,
    selection_generation: Arc<AtomicU64>,
    pending_selection: Option<PendingSelection>,
    pending_start: Option<u64>,
    proxy_node_tags: Vec<String>,
    proxy_node_names: HashMap<String, String>,
    restart_attempts: u32,
    restart_at: Option<Instant>,
    stable_reset_at: Option<Instant>,
    proxy: Option<RunningProxy>,
    binary_manager: BinaryManager,
    events_tx: UnboundedSender<AppEvent>,
    events_rx: UnboundedReceiver<AppEvent>,
    clients: HashMap<u64, ClientHandle>,
    help_return: Option<Page>,
    setup_return: Option<Page>,
}

impl App {
    fn edit_form(&self) -> Option<&EditForm> {
        match &self.overlay {
            Some(Overlay::Edit(form)) => Some(form),
            _ => None,
        }
    }

    fn edit_form_mut(&mut self) -> Option<&mut EditForm> {
        match &mut self.overlay {
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
    pub fn log_detail(&self) -> Option<&LogLine> {
        match &self.overlay {
            Some(Overlay::LogDetail { line, .. }) => Some(line),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn log_detail_scroll(&self) -> usize {
        match &self.overlay {
            Some(Overlay::LogDetail { scroll, .. }) => *scroll,
            _ => 0,
        }
    }

    fn close_overlay(&mut self) {
        self.overlay = None;
        self.log_detail_max_scroll = 0;
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
        let page = if locator.is_none() {
            Page::Setup
        } else {
            Page::Dashboard
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
        let has_enabled = config.enabled_subscriptions().next().is_some();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let mut app = Self {
            config,
            paths,
            logger,
            page,
            dashboard_scroll: 0,
            dashboard_max_scroll: 0,
            status: ProxyStatus::Stopped,
            nodes: cached_nodes,
            node_sort: NodeSort::default(),
            subscriptions: cached_subscriptions,
            selected: 0,
            page_selection: [0; 8],
            metrics: MetricsSnapshot::default(),
            node_delays: HashMap::new(),
            delay_testing: HashSet::new(),
            up_speed: 0.0,
            down_speed: 0.0,
            last_totals: None,
            local_ips: netinfo::local_addresses(false),
            proxy_ips: Vec::new(),
            binary_version,
            binary_verified,
            binary_installed,
            latest_binary_version: None,
            previous_binary_available: false,
            previous_binary_version: None,
            update_check_in_flight: false,
            last_update_check: None,
            setup,
            install_in_flight: false,
            pending_restart_subscription: None,
            overlay: None,
            log_detail_max_scroll: 0,
            subscription_delete: None,
            log_source: LogSource::All,
            log_filter: LogLevel::Info,
            log_follow: true,
            log_timestamp: LogTimestamp::Time,
            last_error: None,
            message: None,
            message_at: None,
            restart_confirm: false,
            rollback_confirm: false,
            restart_in_progress: false,
            restart_after_stop: false,
            managed: false,
            service: None,
            should_quit: false,
            help_scroll: 0,
            help_max_scroll: 0,
            last_local_ips: Instant::now(),
            last_proxy_ips: Instant::now(),
            proxy_ips_in_flight: false,
            proxy_ip_backoff_secs: 5,
            delay_sweep_running: false,
            last_delay_sweep: None,
            health_failures: 0,
            health_failure_tag: None,
            health_probe_in_flight: false,
            health_generation: 0,
            last_health_probe: None,
            last_health_activity: None,
            health_connection_ids: HashSet::new(),
            last_metrics_at: None,
            recovery_in_flight: false,
            last_recovery: None,
            health_pause_until: None,
            recovery_refresh_pending: false,
            last_recovery_refresh: None,
            started_at: None,
            metrics_task: None,
            viewer_active: Arc::new(AtomicBool::new(false)),
            metrics_wake: Arc::new(Notify::new()),
            proxies_refresh: Arc::new(AtomicBool::new(false)),
            refreshing_subscriptions: false,
            subscription_seq: 0,
            subscription_refresh_seq: HashMap::new(),
            last_subscription_refresh: None,
            preferred_node: None,
            runtime_has_auto: false,
            selection_generation: Arc::new(AtomicU64::new(0)),
            pending_selection: None,
            pending_start: None,
            proxy_node_tags: Vec::new(),
            proxy_node_names: HashMap::new(),
            restart_attempts: 0,
            restart_at: None,
            stable_reset_at: None,
            proxy: None,
            binary_manager,
            events_tx,
            events_rx,
            clients: HashMap::new(),
            help_return: None,
            setup_return: None,
        };
        if has_enabled {
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
            .message
            .clone()
            .filter(|_| self.message_at.is_none_or(|at| at.elapsed() < TOAST_TTL))
            .map(|message| crate::text::sanitize_display(&message));
        let filtered = if self.page == Page::Nodes {
            crate::selection::filter_node_tags(
                &self.nodes,
                &self.config.keywords_include,
                &self.config.keywords_exclude,
            )
        } else {
            Vec::new()
        };
        let mut config = self.config.clone();
        config.subscriptions.clear();
        config.lan_username = crate::text::sanitize_display(&config.lan_username);
        config.keywords_include = config
            .keywords_include
            .iter()
            .map(|keyword| crate::text::sanitize_display(keyword))
            .collect();
        config.keywords_exclude = config
            .keywords_exclude
            .iter()
            .map(|keyword| crate::text::sanitize_display(keyword))
            .collect();
        config.test_url = crate::text::sanitize_display(&config.test_url);
        let lan_password_set = !config.lan_password.is_empty();
        config.lan_password.clear();
        let binary_status = if !self.binary_installed {
            BinaryStatus::NotInstalled
        } else if let (Some(latest), Some(current)) = (
            self.latest_binary_version.as_deref(),
            self.binary_version.as_deref(),
        ) {
            if is_newer_version(latest, current) {
                BinaryStatus::UpgradeAvailable
            } else {
                BinaryStatus::Installed
            }
        } else {
            BinaryStatus::Installed
        };
        let tab_page = match self.page {
            Page::Help => self.help_return.unwrap_or(Page::Dashboard),
            Page::Setup => self.setup_return.unwrap_or(Page::Dashboard),
            _ => self.page,
        };
        let tab_page = if tab_page == Page::Setup {
            self.setup_return.unwrap_or(Page::Dashboard)
        } else {
            tab_page
        };
        View {
            config,
            lan_password_set,
            page: self.page,
            tab_page,
            dashboard_scroll: self.dashboard_scroll,
            status: self.status.clone(),
            metrics_stale: self.status == ProxyStatus::Running
                && self.proxy.is_some()
                && self
                    .last_metrics_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(5)),
            nodes: self
                .nodes
                .iter()
                .map(|node| crate::view::NodeView {
                    tag: node.tag.clone(),
                    name: sanitize_display(&node.name),
                    source: sanitize_display(&node.source),
                    protocol: sanitize_display(&node.protocol),
                })
                .collect(),
            auto_tags: filtered.iter().cloned().collect(),
            auto_candidates: filtered.len(),
            subscriptions: self
                .subscriptions
                .iter()
                .map(|runtime| crate::view::SubscriptionView {
                    name: sanitize_display(&runtime.config.name),
                    url: sanitize_display(&subscription::redact_url(&runtime.config.url)),
                    enabled: runtime.config.enabled,
                    node_count: runtime.nodes.len(),
                    unsupported_count: runtime.unsupported.len(),
                    last_updated: runtime.last_updated.clone(),
                    error: runtime.error.as_deref().map(sanitize_display),
                    stale: runtime.stale,
                })
                .collect(),
            selected: self.selected,
            preferred_node: self.preferred_node.clone(),
            metrics: self.metrics.clone(),
            node_delays: self.node_delays.clone(),
            delay_testing: self.delay_testing.clone(),
            up_speed: self.up_speed,
            down_speed: self.down_speed,
            local_ips: self.local_ips.clone(),
            proxy_ips: self.proxy_ips.clone(),
            binary_version: self.binary_version.as_deref().map(sanitize_display),
            binary_verified: self.binary_verified,
            binary_status,
            latest_binary_version: self.latest_binary_version.as_deref().map(sanitize_display),
            previous_binary_available: self.previous_binary_available,
            previous_binary_version: self
                .previous_binary_version
                .as_deref()
                .map(sanitize_display),
            setup: {
                let mut setup = self.setup.clone();
                setup.message = setup.message.as_deref().map(sanitize_display);
                for candidate in &mut setup.candidates {
                    candidate.label = sanitize_display(&candidate.label);
                }
                setup
            },
            overlay: self.overlay.clone(),
            subscription_delete: self.subscription_delete.is_some(),
            log_source: self.log_source,
            log_filter: self.log_filter,
            log_follow: self.log_follow,
            log_timestamp: self.log_timestamp,
            logs: if self.page == Page::Logs {
                self.log_lines()
            } else {
                Vec::new()
            },
            log_file: self.logger.current_file(),
            last_error: self.last_error.as_deref().map(sanitize_display),
            toast,
            restart_confirm: self.restart_confirm,
            rollback_confirm: self.rollback_confirm,
            restart_in_progress: self.restart_in_progress,
            quitting: self.should_quit,
            help_scroll: self.help_scroll,
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
        if let Some(task) = self.metrics_task.take() {
            task.abort();
        }
        if let Some(proxy) = self.proxy.take() {
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
            AppEvent::Subscriptions { seq, runtimes } => {
                self.refreshing_subscriptions = false;
                let recovery_refresh = self.recovery_refresh_pending;
                self.recovery_refresh_pending = false;
                self.last_subscription_refresh = Some(Instant::now());
                let mut accepted: HashMap<String, SubscriptionRuntime> = runtimes
                    .into_iter()
                    .filter(|runtime| {
                        self.subscription_refresh_seq.get(&runtime.config.id) == Some(&seq)
                    })
                    .map(|runtime| (runtime.config.id.clone(), runtime))
                    .collect();
                let mut ordered = Vec::with_capacity(self.config.subscriptions.len());
                for config in &self.config.subscriptions {
                    let mut runtime = accepted
                        .remove(&config.id)
                        .or_else(|| {
                            self.subscriptions
                                .iter()
                                .find(|runtime| runtime.config.id == config.id)
                                .cloned()
                        })
                        .unwrap_or_else(|| empty_runtime(config.clone()));
                    runtime.config = config.clone();
                    ordered.push(runtime);
                }
                self.subscription_refresh_seq.retain(|id, _| {
                    self.config
                        .subscriptions
                        .iter()
                        .any(|config| &config.id == id)
                });
                let selected_before = self.selected;
                let selected_tag = if self.page == Page::Nodes {
                    self.nodes.get(selected_before).map(|node| node.tag.clone())
                } else {
                    None
                };
                let node_count: usize = ordered.iter().map(|item| item.nodes.len()).sum();
                self.nodes = merge_nodes(&ordered);
                self.prune_node_state();
                self.subscriptions = ordered;
                self.sync_preferred_node();
                self.selected = selected_tag
                    .and_then(|tag| self.nodes.iter().position(|node| node.tag == tag))
                    .unwrap_or_else(|| {
                        selected_before.min(self.max_selection_for(self.page).saturating_sub(1))
                    });
                if self.page == Page::Logs && self.log_follow {
                    self.selected = self.max_selection_for(Page::Logs).saturating_sub(1);
                }
                self.sort_nodes_if_visible();
                self.save_subscription_cache();
                self.refresh_last_error_from_subscriptions();
                self.notify(format!(
                    "{}: {node_count}",
                    self.text().msg_subscriptions_refreshed
                ));
                self.logger.app(
                    LogLevel::Info,
                    format!("subscriptions refreshed: {node_count} nodes"),
                );
                if self.pending_restart_subscription.is_some() {
                    self.pending_restart_subscription = None;
                    self.auto_restart_after_change();
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
                if self.subscription_refresh_seq.get(&id) != Some(&seq) {
                    return;
                }
                let mut runtime = runtime;
                runtime.config = config;
                if let Some(existing) = self
                    .subscriptions
                    .iter_mut()
                    .find(|item| item.config.id == id)
                {
                    *existing = runtime.clone();
                } else {
                    self.subscriptions.push(runtime.clone());
                }
                self.nodes = merge_nodes(&self.subscriptions);
                self.prune_node_state();
                self.sync_preferred_node();
                self.sort_nodes_if_visible();
                self.save_subscription_cache();
                self.refresh_last_error_from_subscriptions();
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
                if self.pending_restart_subscription.as_deref() == Some(id.as_str()) {
                    self.pending_restart_subscription = None;
                    self.auto_restart_after_change();
                }
            }
            AppEvent::DownloadDone(result) => {
                self.setup.downloading = false;
                match result {
                    Ok(metadata) => {
                        self.binary_version = Some(metadata.version.clone());
                        self.binary_verified = Some(metadata.verified);
                        self.binary_installed = true;
                        self.latest_binary_version = Some(metadata.version.clone());
                        self.last_update_check = Some(Instant::now());
                        self.notify(format!(
                            "{} {}",
                            self.text().msg_binary_installed,
                            metadata.version
                        ));
                        self.setup.message = None;
                        self.refresh_previous_binary_state();
                        self.switch_page(Page::Dashboard);
                        self.auto_restart_after_change();
                    }
                    Err(err) => {
                        self.setup.message = Some(format!("{}: {err}", self.text().setup_failed));
                        self.last_error = Some(err);
                    }
                }
            }
            AppEvent::InstallDone(result) => {
                self.install_in_flight = false;
                self.setup.installing = false;
                match result {
                    Ok(metadata) => {
                        self.binary_version = Some(metadata.version.clone());
                        self.binary_verified = Some(metadata.verified);
                        self.binary_installed = true;
                        self.latest_binary_version = None;
                        self.last_update_check = None;
                        self.notify(format!(
                            "{} {}",
                            self.text().msg_binary_installed,
                            metadata.version
                        ));
                        self.setup.message = None;
                        self.setup.pending_install = None;
                        self.refresh_previous_binary_state();
                        self.switch_page(Page::Dashboard);
                        self.maybe_check_binary_update();
                        self.auto_restart_after_change();
                    }
                    Err(err) => {
                        self.setup.message = Some(format!("{}: {err}", self.text().setup_failed));
                        self.last_error = Some(err);
                    }
                }
            }
            AppEvent::ProxyStarted(result) => match result {
                Ok(proxy) => {
                    let clash = proxy.clash.clone();
                    let ipv6 = proxy.ipv6;
                    self.proxy = Some(proxy);
                    self.status = ProxyStatus::Running;
                    self.restart_in_progress = false;
                    self.started_at = Some(Instant::now());
                    self.restart_at = None;
                    self.stable_reset_at = Some(Instant::now() + Duration::from_secs(60));
                    self.last_totals = None;
                    self.up_speed = 0.0;
                    self.down_speed = 0.0;
                    self.last_error = None;
                    self.reset_health_state();
                    self.proxy_ip_backoff_secs = 5;
                    self.start_metrics(clash.port, clash.secret.clone());
                    self.apply_pending_selection(clash.port, clash.secret);
                    self.refresh_proxy_ips();
                    if self.page == Page::Nodes {
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
                    self.status = ProxyStatus::Failed;
                    self.restart_in_progress = false;
                    self.runtime_has_auto = false;
                    self.proxy_node_tags.clear();
                    self.proxy_node_names.clear();
                    self.stable_reset_at = None;
                    self.last_error = Some(err);
                    self.logger.app(LogLevel::Error, "sing-box start failed");
                    let detail = self.last_error.clone().unwrap_or_default();
                    self.ack_pending_start(false, format!("proxy start failed: {detail}"));
                    self.schedule_restart();
                }
            },
            AppEvent::ProxyStopped => {
                self.status = ProxyStatus::Stopped;
                self.runtime_has_auto = false;
                self.proxy_node_tags.clear();
                self.proxy_node_names.clear();
                self.started_at = None;
                self.restart_attempts = 0;
                self.restart_at = None;
                self.stable_reset_at = None;
                self.restart_confirm = false;
                self.reset_metrics();
                if self.restart_after_stop {
                    self.restart_after_stop = false;
                    if self.start_proxy().is_err() {
                        self.restart_in_progress = false;
                    }
                } else {
                    self.restart_in_progress = false;
                }
            }
            AppEvent::Metrics(metrics) => {
                let now = Instant::now();
                if let Some((up, down, at)) = self.last_totals {
                    let seconds = now.duration_since(at).as_secs_f64();
                    if seconds >= 0.2 {
                        self.up_speed = metrics.upload_total.saturating_sub(up) as f64 / seconds;
                        self.down_speed =
                            metrics.download_total.saturating_sub(down) as f64 / seconds;
                    }
                }
                self.last_totals = Some((metrics.upload_total, metrics.download_total, now));
                self.last_metrics_at = Some(now);
                let new_connection = metrics
                    .connections
                    .iter()
                    .any(|connection| !self.health_connection_ids.contains(&connection.id));
                self.health_connection_ids = metrics
                    .connections
                    .iter()
                    .map(|connection| connection.id.clone())
                    .collect();
                if new_connection && !self.proxy_ips_in_flight && !self.health_probe_in_flight {
                    self.last_health_activity = Some(now);
                }
                for (tag, delay) in &metrics.node_delays {
                    self.node_delays.insert(tag.clone(), *delay);
                }
                self.metrics = metrics;
                self.sort_nodes_if_visible();
            }
            AppEvent::HealthProbe {
                generation,
                tag,
                outcome,
            } => {
                if generation != self.health_generation {
                    return;
                }
                self.health_probe_in_flight = false;
                if self.status != ProxyStatus::Running {
                    self.reset_health_failures();
                    return;
                }
                match outcome {
                    HealthOutcome::Healthy(delay) => {
                        self.health_failures = 0;
                        self.health_failure_tag = None;
                        self.node_delays.insert(tag, delay);
                        self.sort_nodes_if_visible();
                    }
                    HealthOutcome::Failed => {
                        if self.health_target().as_deref() != Some(tag.as_str()) {
                            self.reset_health_failures();
                            return;
                        }
                        if self.health_failure_tag.as_deref() != Some(tag.as_str()) {
                            self.health_failure_tag = Some(tag.clone());
                            self.health_failures = 0;
                        }
                        self.health_failures += 1;
                        if self.health_failures >= HEALTH_FAILURE_THRESHOLD
                            && !self.recovery_in_flight
                            && !self.recovery_refresh_pending
                            && self
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
                if generation != self.health_generation {
                    return;
                }
                self.recovery_in_flight = false;
                if self.status != ProxyStatus::Running || self.proxy.is_none() {
                    return;
                }
                if self.health_target().as_deref() != Some(trigger.as_str()) {
                    self.reset_health_failures();
                    self.logger.app(
                        LogLevel::Info,
                        "health failover: selection changed before recovery completed",
                    );
                    return;
                }
                let Some(target) =
                    choose_recovery_target(self.metrics.selection.selected.as_deref(), &healthy)
                else {
                    self.on_no_healthy_nodes();
                    return;
                };
                let Some(proxy) = self.proxy.as_ref() else {
                    return;
                };
                let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
                let (tag, name) = match &target {
                    RecoveryTarget::Auto => {
                        self.preferred_node = None;
                        ("auto".to_string(), String::new())
                    }
                    RecoveryTarget::Node(tag) => {
                        let name = self
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
                self.selection_generation.fetch_add(1, Ordering::Relaxed);
                self.health_failures = 0;
                self.health_failure_tag = None;
                self.last_error = None;
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
                tokio::spawn(async move {
                    match client.select(&tag).await {
                        Ok(()) => {
                            let _ = tx.send(AppEvent::Info(label));
                        }
                        Err(err) => {
                            let _ = tx.send(AppEvent::Error(err.to_string()));
                        }
                    }
                });
            }
            AppEvent::ProxyIps(ips) => {
                self.proxy_ips_in_flight = false;
                if self.proxy.is_none() {
                    return;
                }
                if ips.is_empty() {
                    self.proxy_ips.clear();
                    self.proxy_ip_backoff_secs = (self.proxy_ip_backoff_secs * 2).min(60);
                } else {
                    self.proxy_ips = ips;
                    self.proxy_ip_backoff_secs = 5;
                }
            }
            AppEvent::NodeDelay { tag, delay } => {
                self.delay_testing.remove(&tag);
                if self.proxy.is_none() {
                    return;
                }
                if let Some(delay) = delay.filter(|delay| *delay > 0) {
                    self.node_delays.insert(tag, delay);
                    self.sort_nodes_if_visible();
                }
            }
            AppEvent::NodeSelected(tag) => {
                if self.nodes.iter().any(|node| node.tag == tag) {
                    self.preferred_node = Some(tag);
                }
            }
            AppEvent::SelectionApplied => self.request_metrics_refresh(),
            AppEvent::NodeDelaySweepDone => {
                self.delay_sweep_running = false;
                self.delay_testing.clear();
                self.last_delay_sweep = Some(Instant::now());
            }
            AppEvent::DashboardViewport(max_scroll) => {
                self.dashboard_max_scroll = max_scroll;
                self.dashboard_scroll = self.dashboard_scroll.min(max_scroll);
            }
            AppEvent::HelpViewport(max_scroll) => {
                self.help_max_scroll = max_scroll;
                self.help_scroll = self.help_scroll.min(max_scroll);
            }
            AppEvent::LogDetailViewport(max_scroll) => {
                self.log_detail_max_scroll = max_scroll;
                if let Some(Overlay::LogDetail { scroll, .. }) = self.overlay.as_mut() {
                    *scroll = (*scroll).min(max_scroll);
                }
            }
            AppEvent::UpdateCheckDone(result) => {
                self.update_check_in_flight = false;
                match result {
                    Ok(version) => {
                        self.logger.app(
                            LogLevel::Info,
                            format!("latest sing-box release: {version}"),
                        );
                        self.latest_binary_version = Some(version);
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
                self.setup.rollback_in_flight = false;
                self.rollback_confirm = false;
                match result {
                    Ok(metadata) => {
                        self.binary_version = Some(metadata.version.clone());
                        self.binary_verified = Some(metadata.verified);
                        self.binary_installed = true;
                        self.setup.message = None;
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
                        self.setup.message =
                            Some(format!("{}: {err}", self.text().setup_switch_failed));
                        self.last_error = Some(err);
                    }
                }
            }
            AppEvent::Info(message) => {
                self.notify(message.clone());
                self.logger.app(LogLevel::Info, message);
            }
            AppEvent::Error(message) => {
                self.last_error = Some(message.clone());
                self.logger.app(LogLevel::Error, message);
            }
            AppEvent::ClientAttached { id, viewer, tx } => {
                if viewer && self.clients.values().any(|client| client.viewer) {
                    let _ = tx.try_send(ServerMsg::Busy);
                } else {
                    self.clients.insert(id, ClientHandle { viewer, tx });
                    if viewer {
                        self.viewer_active.store(true, Ordering::Relaxed);
                        self.metrics_wake.notify_one();
                        if self.page == Page::Logs && self.log_follow {
                            self.selected = self.log_count().saturating_sub(1);
                        }
                        self.sort_nodes_if_visible();
                        self.broadcast_view();
                    }
                }
            }
            AppEvent::ClientDetached(id) => {
                if let Some(client) = self.clients.remove(&id) {
                    if client.viewer {
                        self.viewer_active.store(false, Ordering::Relaxed);
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
                self.ack(client_id, true, "stopping");
                self.should_quit = true;
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
            proxy: self.status.clone(),
            uptime_secs: self
                .started_at
                .map(|started| started.elapsed().as_secs())
                .unwrap_or(0),
            listen: self.listen_addresses(),
            current_node: self.current_node(),
        };
        let _ = client.tx.try_send(ServerMsg::Status(Box::new(status)));
    }

    fn ack_pending_start(&mut self, ok: bool, message: impl Into<String>) {
        if let Some(client_id) = self.pending_start.take() {
            self.ack(client_id, ok, message);
        }
    }

    fn handle_start_request(&mut self, client_id: u64) {
        if self.status == ProxyStatus::Running {
            self.ack(client_id, true, "proxy is already running");
            return;
        }
        if matches!(self.status, ProxyStatus::Starting | ProxyStatus::Stopping) {
            self.ack(client_id, false, "proxy state is changing");
            return;
        }
        if self.nodes.is_empty() {
            self.pending_start = Some(client_id);
            if self.refreshing_subscriptions {
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
        if self.nodes.is_empty() {
            if self.refreshing_subscriptions {
                return;
            }
            self.ack_pending_start(false, "no nodes available");
            return;
        }
        if matches!(self.status, ProxyStatus::Starting | ProxyStatus::Stopping) {
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
        self.viewer_active.load(Ordering::Relaxed)
    }

    fn request_metrics_refresh(&self) {
        self.proxies_refresh.store(true, Ordering::Relaxed);
        self.metrics_wake.notify_one();
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

    fn publish_view(&mut self) {
        if self.has_viewer() {
            self.broadcast_view();
        }
    }

    pub fn notify(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
        self.message_at = Some(Instant::now());
    }

    async fn handle_key(&mut self, key: KeyEvent) {
        if self.restart_in_progress {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.overlay.is_some() {
                self.close_overlay();
                return;
            }
            self.restart_confirm = false;
            self.rollback_confirm = false;
            self.subscription_delete = None;
            self.setup.pending_install = None;
            self.setup.message = None;
            self.detach_clients();
            return;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        if let Some(kind) = self.overlay.as_ref().map(Overlay::kind) {
            self.handle_overlay_key(kind, key);
            return;
        }
        if self.restart_confirm {
            match key.code {
                KeyCode::Char('R') => {
                    self.restart_confirm = false;
                    self.perform_restart();
                    return;
                }
                KeyCode::Esc => {
                    self.restart_confirm = false;
                    return;
                }
                _ => {
                    self.restart_confirm = false;
                    return;
                }
            }
        }
        if self.rollback_confirm {
            match key.code {
                KeyCode::Char('b') => {
                    self.rollback_confirm = false;
                    self.perform_rollback();
                    return;
                }
                KeyCode::Esc => {
                    self.rollback_confirm = false;
                    return;
                }
                _ => {
                    self.rollback_confirm = false;
                    return;
                }
            }
        }
        if self.setup.pending_install.is_some() {
            if key.code == KeyCode::Enter {
                self.install_selected().await;
            } else {
                self.setup.pending_install = None;
                self.setup.message = None;
            }
            return;
        }
        if let Some(id) = self.subscription_delete.clone() {
            match key.code {
                KeyCode::Char('d') => {
                    self.subscription_delete = None;
                    self.delete_subscription(&id);
                    return;
                }
                KeyCode::Esc => {
                    self.subscription_delete = None;
                    return;
                }
                _ => {
                    self.subscription_delete = None;
                    return;
                }
            }
        }
        if self.page == Page::Dashboard {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.dashboard_scroll = self
                        .dashboard_scroll
                        .saturating_add(1)
                        .min(self.dashboard_max_scroll);
                    return;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.dashboard_scroll = self.dashboard_scroll.saturating_sub(1);
                    return;
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    self.dashboard_scroll = 0;
                    return;
                }
                KeyCode::Char('G') | KeyCode::End => {
                    self.dashboard_scroll = self.dashboard_max_scroll;
                    return;
                }
                _ => {}
            }
        }
        if self.page == Page::Help {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.help_scroll = self.help_scroll.saturating_add(1).min(self.help_max_scroll);
                    return;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                    return;
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    self.help_scroll = 0;
                    return;
                }
                KeyCode::Char('G') | KeyCode::End => {
                    self.help_scroll = self.help_max_scroll;
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('q') => self.detach_clients(),
            KeyCode::Esc => match self.page {
                Page::Help => self.return_from_help(),
                Page::Setup => self.return_from_setup(),
                _ => {}
            },
            KeyCode::Char('l') | KeyCode::Right => {
                let target = if MAIN_PAGES.contains(&self.page) {
                    self.page.next_main()
                } else {
                    Page::Dashboard
                };
                self.switch_page(target);
            }
            KeyCode::Char('h') | KeyCode::Left => {
                let target = if MAIN_PAGES.contains(&self.page) {
                    self.page.previous_main()
                } else {
                    Page::Settings
                };
                self.switch_page(target);
            }
            KeyCode::Char('?') => self.toggle_help(),
            KeyCode::Char('L') => self.toggle_language(),
            KeyCode::Char('s') => self.start_or_stop(),
            KeyCode::Char('R') => self.request_restart(),
            KeyCode::Char('r') => self.refresh_current().await,
            KeyCode::Char('d') if self.page == Page::Setup => self.start_download(),
            KeyCode::Char('b') if self.page == Page::Setup => self.request_rollback(),
            KeyCode::Char('p') if self.page == Page::Setup => self.copy_binary_path(),
            KeyCode::Char('d') if self.page == Page::Subscriptions => {
                self.request_delete_subscription();
            }
            KeyCode::Char('n') if self.page == Page::Subscriptions => {
                self.overlay = Some(Overlay::Edit(EditForm::Subscription(
                    SubscriptionForm::new(),
                )));
                self.message = None;
            }
            KeyCode::Char('e') if self.page == Page::Subscriptions => {
                self.open_edit_subscription_form();
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.jump_selection(false),
            KeyCode::Char('G') | KeyCode::End => self.jump_selection(true),
            KeyCode::Char('f') if self.page == Page::Logs => self.toggle_log_follow(),
            KeyCode::Char('d') if self.page == Page::Logs => self.cycle_log_timestamp(),
            KeyCode::Char('v') if self.page == Page::Logs => self.cycle_log_filter(),
            KeyCode::Char('x') if self.page == Page::Logs => self.cycle_log_source(),
            KeyCode::Char('c') if self.page == Page::Logs => {
                self.logger.clear_view();
                self.selected = 0;
                self.log_follow = true;
            }
            KeyCode::Char('a') if self.page == Page::Nodes => self.select_node_auto(),
            KeyCode::Char('n') if self.page == Page::Nodes => self.toggle_node_sort(),
            KeyCode::Char('t') if self.page == Page::Nodes => self.test_node_delay(),
            KeyCode::Enter => self.handle_enter().await,
            _ => {}
        }
    }

    async fn handle_enter(&mut self) {
        match self.page {
            Page::Dashboard => {
                if self.last_error.is_some() {
                    self.switch_page(Page::Logs);
                }
            }
            Page::Setup => self.install_selected().await,
            Page::Nodes => self.select_node().await,
            Page::Subscriptions => self.open_edit_subscription_form(),
            Page::Connections => self.copy_connection_target(),
            Page::Logs => self.open_log_detail(),
            Page::Settings => self.activate_setting(),
            _ => {}
        }
    }

    fn toggle_help(&mut self) {
        if self.page == Page::Help {
            self.return_from_help();
            return;
        }
        self.help_return = Some(self.page);
        self.help_scroll = 0;
        self.switch_page(Page::Help);
    }

    fn return_from_help(&mut self) {
        let target = self.help_return.take().unwrap_or(Page::Dashboard);
        self.help_scroll = 0;
        self.switch_page(target);
    }

    fn return_from_setup(&mut self) {
        let target = self.setup_return.take().unwrap_or(Page::Dashboard);
        self.setup.pending_install = None;
        self.switch_page(target);
    }

    fn open_setup(&mut self) {
        if self.page != Page::Setup {
            self.switch_page(Page::Setup);
        }
        self.setup.pending_install = None;
        self.setup.candidates = self.binary_manager.scan_inbox().unwrap_or_default();
        self.setup.message = None;
        self.selected = 0;
        self.refresh_previous_binary_state();
        self.maybe_check_binary_update();
    }

    fn copy_binary_path(&mut self) {
        let path = self.paths.binary().display().to_string();
        self.broadcast(ServerMsg::Copy { text: path });
        self.notify(self.text().settings_copied);
    }

    fn copy_connection_target(&mut self) {
        let selected = self
            .selected
            .min(self.metrics.connections.len().saturating_sub(1));
        let Some(connection) = self.metrics.connections.get(selected) else {
            return;
        };
        let Some(target) = connection.target() else {
            return;
        };
        self.broadcast(ServerMsg::Copy { text: target });
        self.notify(self.text().settings_copied);
    }

    fn refresh_previous_binary_state(&mut self) {
        let available = self.paths.previous_binary().is_file();
        self.previous_binary_available = available;
        self.previous_binary_version = if available {
            self.binary_manager
                .previous_metadata()
                .map(|metadata| metadata.version)
        } else {
            None
        };
    }

    fn maybe_check_binary_update(&mut self) {
        if self.update_check_in_flight || !self.binary_installed {
            return;
        }
        if self
            .last_update_check
            .is_some_and(|at| at.elapsed() < UPDATE_CHECK_INTERVAL)
        {
            return;
        }
        self.update_check_in_flight = true;
        self.last_update_check = Some(Instant::now());
        let manager = self.binary_manager.clone();
        let config = self.config.clone();
        let tx = self.events_tx.clone();
        tokio::spawn(async move {
            let result = manager
                .latest_version(&config)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::UpdateCheckDone(result));
        });
    }

    fn open_log_detail(&mut self) {
        if let Some(line) = self.log_lines().get(self.selected).cloned() {
            self.overlay = Some(Overlay::LogDetail { line, scroll: 0 });
            self.log_detail_max_scroll = 0;
        }
    }

    fn detach_clients(&mut self) {
        self.broadcast(ServerMsg::Detach);
        self.clients.retain(|_, client| !client.viewer);
    }

    fn switch_page(&mut self, page: Page) {
        if self.page == page {
            return;
        }
        if page == Page::Setup && self.page != Page::Setup && self.page != Page::Help {
            self.setup_return = Some(self.page);
        }
        if self.page == Page::Setup && page != Page::Setup && page != Page::Help {
            self.setup_return = None;
        }
        self.page_selection[self.page.index()] = self.selected;
        if self.page == Page::Help && page != Page::Help {
            self.help_return = None;
        }
        self.close_overlay();
        self.page = page;
        self.subscription_delete = None;
        self.rollback_confirm = false;
        let max = self.max_selection_for(page);
        if page == Page::Logs && self.log_follow {
            self.selected = max.saturating_sub(1);
        } else {
            self.selected = self.page_selection[page.index()].min(max.saturating_sub(1));
        }
        if page == Page::Nodes {
            self.sort_nodes_preserving_selection();
            self.maybe_test_node_delays(false);
        }
        if matches!(page, Page::Settings | Page::Setup) {
            self.maybe_check_binary_update();
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.page == Page::Logs {
            self.log_follow = false;
        }
        let max = self.max_selection();
        if max == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as i32 + delta;
        self.selected = next.clamp(0, max as i32 - 1) as usize;
        if self.page == Page::Logs && self.selected == max - 1 {
            self.log_follow = true;
        }
    }

    fn jump_selection(&mut self, bottom: bool) {
        if self.page == Page::Logs {
            self.log_follow = bottom;
        }
        let max = self.max_selection();
        self.selected = if bottom { max.saturating_sub(1) } else { 0 };
    }

    fn max_selection(&self) -> usize {
        self.max_selection_for(self.page)
    }

    fn max_selection_for(&self, page: Page) -> usize {
        match page {
            Page::Setup => self.setup.candidates.len(),
            Page::Nodes => self.nodes.len(),
            Page::Connections => self.metrics.connections.len(),
            Page::Subscriptions => self.subscriptions.len(),
            Page::Logs => self.log_count(),
            Page::Settings => settings_items(self.config.allow_lan).len(),
            _ => 0,
        }
    }

    fn toggle_node_sort(&mut self) {
        self.node_sort = match self.node_sort {
            NodeSort::AutoLatency => NodeSort::Name,
            NodeSort::Name => NodeSort::AutoLatency,
        };
        self.sort_nodes_preserving_selection();
    }

    fn sort_nodes_if_visible(&mut self) {
        if self.has_viewer() && self.page == Page::Nodes {
            self.sort_nodes_preserving_selection();
        }
    }

    fn sort_nodes_preserving_selection(&mut self) {
        let selected_tag = self.nodes.get(self.selected).map(|node| node.tag.clone());
        let auto_tags: HashSet<String> = crate::selection::filter_node_tags(
            &self.nodes,
            &self.config.keywords_include,
            &self.config.keywords_exclude,
        )
        .into_iter()
        .collect();
        sort_nodes(
            &mut self.nodes,
            self.node_sort,
            &auto_tags,
            &self.node_delays,
        );
        self.selected = selected_tag
            .and_then(|tag| self.nodes.iter().position(|node| node.tag == tag))
            .unwrap_or_else(|| self.selected.min(self.nodes.len().saturating_sub(1)));
    }

    pub fn log_lines(&self) -> Vec<LogLine> {
        self.logger
            .recent_filtered(500, self.log_filter, self.log_source)
    }

    fn log_count(&self) -> usize {
        self.logger
            .count_recent(500, self.log_filter, self.log_source)
    }

    fn toggle_log_follow(&mut self) {
        self.log_follow = !self.log_follow;
        if self.log_follow {
            self.selected = self.max_selection().saturating_sub(1);
        }
        let state = if self.log_follow {
            self.text().common_on
        } else {
            self.text().common_off
        };
        self.notify(format!("{}: {state}", self.text().logs_follow));
    }

    fn cycle_log_timestamp(&mut self) {
        self.log_timestamp = self.log_timestamp.next();
    }

    fn cycle_log_filter(&mut self) {
        self.log_filter = self.log_filter.next_filter();
        self.reset_log_selection();
        self.notify(format!(
            "{}: {}",
            self.text().logs_level,
            self.log_filter.label()
        ));
    }

    fn cycle_log_source(&mut self) {
        self.log_source = self.log_source.next();
        self.reset_log_selection();
        self.notify(format!(
            "{}: {}",
            self.text().logs_source,
            self.log_source.label()
        ));
    }

    fn reset_log_selection(&mut self) {
        let max = self.max_selection();
        if self.log_follow {
            self.selected = max.saturating_sub(1);
        } else {
            self.selected = self.selected.min(max.saturating_sub(1));
        }
    }

    async fn refresh_current(&mut self) {
        match self.page {
            Page::Setup => {
                self.setup.candidates = self.binary_manager.scan_inbox().unwrap_or_default();
                self.selected = 0;
                self.setup.message = None;
            }
            Page::Nodes => self.maybe_test_node_delays(true),
            Page::Subscriptions | Page::Dashboard => self.refresh_subscriptions(),
            _ => {
                self.local_ips = netinfo::local_addresses(false);
                self.last_local_ips = Instant::now();
            }
        }
        self.refresh_proxy_ips();
    }

    fn refresh_subscriptions(&mut self) {
        if self.refreshing_subscriptions {
            return;
        }
        let configs: Vec<SubscriptionConfig> =
            self.config.enabled_subscriptions().cloned().collect();
        if configs.is_empty() {
            self.last_error = Some(self.text().msg_no_enabled_subscriptions.to_string());
            return;
        }
        self.refreshing_subscriptions = true;
        self.last_subscription_refresh = Some(Instant::now());
        let seq = self.next_subscription_seq();
        for config in &configs {
            self.subscription_refresh_seq.insert(config.id.clone(), seq);
        }
        let previous = self.subscriptions.clone();
        let tx = self.events_tx.clone();
        let logger = self.logger.clone();
        tokio::spawn(async move {
            logger.app(
                LogLevel::Info,
                format!("refreshing {} subscriptions", configs.len()),
            );
            let runtimes =
                subscription::refresh_all(&configs, &FetchOptions::default(), &previous).await;
            let _ = tx.send(AppEvent::Subscriptions { seq, runtimes });
        });
    }

    fn save_subscription_cache(&self) {
        if let Err(err) =
            subscription::cache::save(&self.paths.subscription_cache(), &self.subscriptions)
        {
            self.logger.app(
                LogLevel::Warn,
                format!("write subscription cache failed: {err}"),
            );
        }
    }

    fn open_edit_subscription_form(&mut self) {
        let Some(runtime) = self.subscriptions.get(self.selected) else {
            self.notify(self.text().subscriptions_empty);
            return;
        };
        self.overlay = Some(Overlay::Edit(EditForm::Subscription(
            SubscriptionForm::edit(&runtime.config),
        )));
        self.message = None;
    }

    fn request_delete_subscription(&mut self) {
        let Some(runtime) = self.subscriptions.get(self.selected) else {
            return;
        };
        self.subscription_delete = Some(runtime.config.id.clone());
        self.message = None;
        self.message_at = None;
    }

    fn delete_subscription(&mut self, id: &str) {
        let mut config = self.config.clone();
        let was_enabled = config
            .subscriptions
            .iter()
            .any(|subscription| subscription.id == id && subscription.enabled);
        let before = config.subscriptions.len();
        config
            .subscriptions
            .retain(|subscription| subscription.id != id);
        if config.subscriptions.len() == before {
            return;
        }
        if let Err(err) = config.save(&self.paths.config_file) {
            self.last_error = Some(err.to_string());
            return;
        }
        self.config = config;
        self.subscription_refresh_seq.remove(id);
        if self.pending_restart_subscription.as_deref() == Some(id) {
            self.pending_restart_subscription = None;
        }
        self.subscriptions.retain(|runtime| runtime.config.id != id);
        self.nodes = merge_nodes(&self.subscriptions);
        self.prune_node_state();
        self.sync_preferred_node();
        self.selected = self
            .selected
            .min(self.subscriptions.len().saturating_sub(1));
        self.save_subscription_cache();
        self.refresh_last_error_from_subscriptions();
        self.notify(self.text().msg_subscription_deleted);
        if was_enabled {
            self.apply_nodes_change_to_proxy();
        }
        self.logger
            .app(LogLevel::Info, format!("subscription deleted: {id}"));
    }

    fn handle_overlay_key(&mut self, kind: OverlayKind, key: KeyEvent) {
        match kind {
            OverlayKind::Subscription => self.handle_form_key(key),
            OverlayKind::Keyword => self.handle_keyword_form_key(key),
            OverlayKind::Settings => self.handle_settings_form_key(key),
            OverlayKind::LogDetail => self.handle_log_detail_key(key),
        }
    }

    fn handle_log_detail_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.close_overlay(),
            KeyCode::Char('j') | KeyCode::Down => self.scroll_log_detail(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_log_detail(-1),
            KeyCode::Char('g') | KeyCode::Home => self.scroll_log_detail_to(0),
            KeyCode::Char('G') | KeyCode::End => {
                self.scroll_log_detail_to(self.log_detail_max_scroll)
            }
            _ => {}
        }
    }

    fn scroll_log_detail(&mut self, delta: i32) {
        let max = self.log_detail_max_scroll;
        if let Some(Overlay::LogDetail { scroll, .. }) = self.overlay.as_mut() {
            *scroll = if delta >= 0 {
                scroll.saturating_add(delta as usize).min(max)
            } else {
                scroll.saturating_sub((-delta) as usize)
            };
        }
    }

    fn scroll_log_detail_to(&mut self, offset: usize) {
        if let Some(Overlay::LogDetail { scroll, .. }) = self.overlay.as_mut() {
            *scroll = offset.min(self.log_detail_max_scroll);
        }
    }

    fn handle_form_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.close_overlay();
                return;
            }
            KeyCode::Enter => {
                self.save_subscription_form();
                return;
            }
            KeyCode::Tab | KeyCode::Down => {
                if let Some(form) = self.subscription_form_mut() {
                    form.focus = (form.focus + 1) % SubscriptionForm::FOCUS_COUNT;
                    form.error = None;
                }
                return;
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Some(form) = self.subscription_form_mut() {
                    form.focus = (form.focus + SubscriptionForm::FOCUS_COUNT - 1)
                        % SubscriptionForm::FOCUS_COUNT;
                    form.error = None;
                }
                return;
            }
            _ => {}
        }
        let Some(form) = self.subscription_form_mut() else {
            return;
        };
        if form.focus == 2 {
            if key.code == KeyCode::Char(' ') {
                form.enabled = !form.enabled;
                form.error = None;
            }
            return;
        }
        let input = if form.focus == 0 {
            &mut form.name
        } else {
            &mut form.url
        };
        if edit_line_input(input, &key) {
            form.error = None;
        }
    }

    fn save_subscription_form(&mut self) {
        let Some(form) = self.subscription_form().cloned() else {
            return;
        };
        let url = form.url.trim_start_end();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            self.set_edit_error(self.text().subscription_form_invalid_url.to_string());
            return;
        }
        let mut name = form.name.trim_start_end();
        if name.is_empty() {
            name = default_subscription_name(&url);
        }
        let id = form
            .editing_id
            .clone()
            .unwrap_or_else(|| format!("s-{:016x}", rand::random::<u64>()));
        let previous = self
            .config
            .subscriptions
            .iter()
            .find(|subscription| subscription.id == id)
            .cloned();
        let was_enabled = previous
            .as_ref()
            .is_some_and(|subscription| subscription.enabled);
        let restart_needed = previous
            .as_ref()
            .is_none_or(|old| old.url != url || old.enabled != form.enabled);
        let mut updated_config = self.config.clone();
        let config = if let Some(existing) = updated_config
            .subscriptions
            .iter_mut()
            .find(|config| config.id == id)
        {
            existing.name = name.clone();
            existing.url = url;
            existing.enabled = form.enabled;
            existing.clone()
        } else {
            let config = SubscriptionConfig {
                id,
                name: name.clone(),
                url,
                enabled: form.enabled,
            };
            updated_config.subscriptions.push(config.clone());
            config
        };
        if let Err(err) = self.save_config_changes(updated_config) {
            self.set_edit_error(err);
            return;
        }
        self.close_overlay();
        let mut saved_message = format!("{}: {name}", self.text().msg_subscription_saved);
        if config.url.starts_with("http://") {
            saved_message.push_str(" · ");
            saved_message.push_str(self.text().subscription_http_warning);
        }
        self.notify(saved_message);
        self.logger.app(
            LogLevel::Info,
            format!(
                "subscription saved: {} (enabled={})",
                config.id, config.enabled
            ),
        );
        if config.enabled {
            if !self
                .subscriptions
                .iter()
                .any(|runtime| runtime.config.id == config.id)
            {
                self.subscriptions.push(empty_runtime(config.clone()));
            }
            if restart_needed {
                self.pending_restart_subscription = Some(config.id.clone());
            }
            self.refresh_subscription(config);
        } else {
            self.subscription_refresh_seq.remove(&config.id);
            let placeholder = empty_runtime(config);
            if let Some(existing) = self
                .subscriptions
                .iter_mut()
                .find(|runtime| runtime.config.id == placeholder.config.id)
            {
                *existing = placeholder;
            } else {
                self.subscriptions.push(placeholder);
            }
            self.nodes = merge_nodes(&self.subscriptions);
            self.sync_preferred_node();
            self.save_subscription_cache();
            self.refresh_last_error_from_subscriptions();
            if was_enabled {
                self.apply_nodes_change_to_proxy();
            }
        }
    }

    /// React to a node-set change. Keeping the old proxy alive is correct when
    /// the new configuration is invalid, but an intentional removal of all
    /// subscriptions should stop the proxy rather than leave stale routing.
    fn apply_nodes_change_to_proxy(&mut self) {
        if self.nodes.is_empty() {
            if self.proxy.is_some() {
                self.stop_proxy();
            }
        } else {
            self.auto_restart_after_change();
        }
    }

    fn open_keyword_form(&mut self, field: KeywordField) {
        self.overlay = Some(Overlay::Edit(EditForm::Keyword(KeywordForm::from_config(
            &self.config,
            field,
        ))));
        self.message = None;
    }

    fn activate_setting(&mut self) {
        let items = settings_items(self.config.allow_lan);
        let Some(item) = items.get(self.selected).copied() else {
            return;
        };
        if item.read_only() {
            let value = self.setting_value(item);
            self.broadcast(ServerMsg::Copy { text: value });
            self.notify(self.text().settings_copied);
            return;
        }
        match item {
            SettingsItem::Language => self.toggle_language(),
            SettingsItem::AllowLan => self.toggle_allow_lan(),
            SettingsItem::LanAuth => self.open_settings_form(),
            SettingsItem::KeywordsInclude => self.open_keyword_form(KeywordField::Include),
            SettingsItem::KeywordsExclude => self.open_keyword_form(KeywordField::Exclude),
            SettingsItem::AppLogLevel => self.cycle_app_log_level(),
            SettingsItem::BinaryInfo => self.open_setup(),
            SettingsItem::Port
            | SettingsItem::DnsServers
            | SettingsItem::TestUrl
            | SettingsItem::ConfigPath
            | SettingsItem::LogFile => {}
        }
    }

    fn setting_value(&self, item: SettingsItem) -> String {
        match item {
            SettingsItem::Port => self.config.port.to_string(),
            SettingsItem::DnsServers => self.config.dns_servers.join(", "),
            SettingsItem::TestUrl => self.config.test_url.clone(),
            SettingsItem::ConfigPath => self.paths.config_file.display().to_string(),
            SettingsItem::LogFile => self.logger.current_file().display().to_string(),
            _ => String::new(),
        }
    }

    fn open_settings_form(&mut self) {
        self.overlay = Some(Overlay::Edit(EditForm::Settings(SettingsForm::new(
            &self.config,
        ))));
        self.message = None;
    }

    fn handle_settings_form_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.close_overlay();
                return;
            }
            KeyCode::Enter => {
                self.save_settings_form();
                return;
            }
            KeyCode::Tab | KeyCode::Down => {
                if let Some(form) = self.settings_form_mut() {
                    form.switch_field(true);
                    form.error = None;
                }
                return;
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Some(form) = self.settings_form_mut() {
                    form.switch_field(false);
                    form.error = None;
                }
                return;
            }
            _ => {}
        }
        let Some(form) = self.settings_form_mut() else {
            return;
        };
        if edit_line_input(form.active_input_mut(), &key) {
            form.error = None;
        }
    }

    fn save_settings_form(&mut self) {
        let Some(form) = self.settings_form().cloned() else {
            return;
        };
        let username = form.username.trim_start_end();
        if username.contains(':') {
            self.set_edit_error(self.text().settings_invalid_username.to_string());
            return;
        }
        let mut config = self.config.clone();
        config.lan_username = username;
        config.lan_password = form.password.value.clone();
        if let Err(err) = self.save_config_changes(config) {
            self.set_edit_error(err);
            return;
        }
        self.close_overlay();
        self.notify_saved_with_restart();
    }

    fn cycle_app_log_level(&mut self) {
        let mut config = self.config.clone();
        config.app_log_level = config.app_log_level.next_filter();
        if let Err(err) = config.save(&self.paths.config_file) {
            self.last_error = Some(err.to_string());
            return;
        }
        self.config = config;
        self.logger
            .set_levels(self.config.app_log_level, self.config.singbox_log_level);
        self.notify(self.text().settings_saved);
    }

    fn notify_saved_with_restart(&mut self) {
        self.notify(self.text().settings_saved);
        self.auto_restart_after_change();
    }

    fn handle_keyword_form_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.close_overlay();
                return;
            }
            KeyCode::Enter => {
                self.save_keyword_form();
                return;
            }
            _ => {}
        }
        let Some(form) = self.keyword_form_mut() else {
            return;
        };
        if edit_line_input(&mut form.value, &key) {
            form.error = None;
        }
    }

    fn save_keyword_form(&mut self) {
        let Some(form) = self.keyword_form().cloned() else {
            return;
        };
        let keywords = parse_keyword_input(&form.value.value);
        let mut config = self.config.clone();
        match form.field {
            KeywordField::Include => config.keywords_include = keywords,
            KeywordField::Exclude => config.keywords_exclude = keywords,
        }
        if let Err(err) = self.save_config_changes(config) {
            self.set_edit_error(err);
            return;
        }
        self.close_overlay();
        let filtered = crate::selection::filter_node_tags(
            &self.nodes,
            &self.config.keywords_include,
            &self.config.keywords_exclude,
        )
        .len();
        let total = self.nodes.len();
        let mut message =
            if self.config.keywords_include.is_empty() && self.config.keywords_exclude.is_empty() {
                format!(
                    "{}: {}",
                    self.text().settings_keywords_saved,
                    self.text().settings_keywords_all
                )
            } else {
                format!(
                    "{}: {} {}/{}",
                    self.text().settings_keywords_saved,
                    self.text().nodes_auto_candidates,
                    filtered,
                    total
                )
            };
        if filtered == 0 && total > 0 {
            message.push_str(" · ");
            message.push_str(self.text().settings_keywords_no_match);
        }
        self.notify(message);
        self.auto_restart_after_change();
        self.logger.app(
            LogLevel::Info,
            format!(
                "node keywords updated: include={} exclude={} auto={filtered}/{total}",
                self.config.keywords_include.len(),
                self.config.keywords_exclude.len()
            ),
        );
    }

    fn prune_node_state(&mut self) {
        let tags: HashSet<String> = self.nodes.iter().map(|node| node.tag.clone()).collect();
        self.node_delays.retain(|tag, _| tags.contains(tag));
        self.delay_testing.retain(|tag| tags.contains(tag));
    }

    fn next_subscription_seq(&mut self) -> u64 {
        self.subscription_seq = self.subscription_seq.wrapping_add(1);
        self.subscription_seq
    }

    fn refresh_last_error_from_subscriptions(&mut self) {
        let errors: Vec<String> = self
            .subscriptions
            .iter()
            .filter_map(|runtime| {
                runtime
                    .error
                    .as_ref()
                    .map(|error| format!("{}: {error}", runtime.config.name))
            })
            .collect();
        if !errors.is_empty() {
            self.last_error = Some(errors.join("; "));
        } else if self.status != ProxyStatus::Failed {
            self.last_error = None;
        }
    }

    fn sync_preferred_node(&mut self) {
        if let Some(tag) = self.preferred_node.clone() {
            if !self.nodes.iter().any(|node| node.tag == tag) {
                self.preferred_node = None;
                self.pending_selection = None;
                self.notify(self.text().msg_preferred_missing);
            }
        }
    }

    fn refresh_subscription(&mut self, config: SubscriptionConfig) {
        // Keep last successful nodes even when the URL changed: a failed
        // refresh of a newly edited URL must not erase the currently working
        // node set and trigger a restart that cannot start.
        let previous = self
            .subscriptions
            .iter()
            .find(|runtime| runtime.config.id == config.id)
            .cloned();
        let seq = self.next_subscription_seq();
        self.subscription_refresh_seq.insert(config.id.clone(), seq);
        let tx = self.events_tx.clone();
        let logger = self.logger.clone();
        tokio::spawn(async move {
            logger.app(
                LogLevel::Info,
                format!("refreshing subscription {}", config.name),
            );
            let runtime =
                subscription::refresh_one(config, &FetchOptions::default(), previous.as_ref())
                    .await;
            let _ = tx.send(AppEvent::SubscriptionUpdated { seq, runtime });
        });
    }

    fn start_or_stop(&mut self) {
        if matches!(self.status, ProxyStatus::Starting | ProxyStatus::Stopping) {
            self.notify(self.text().msg_proxy_busy);
            return;
        }
        if self.proxy.is_some() {
            self.stop_proxy();
            return;
        }
        self.restart_attempts = 0;
        self.restart_at = None;
        self.stable_reset_at = None;
        let _ = self.start_proxy();
    }

    fn request_restart(&mut self) {
        if self.setup.downloading || self.setup.rollback_in_flight || self.install_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        if matches!(self.status, ProxyStatus::Starting | ProxyStatus::Stopping) {
            self.notify(self.text().msg_proxy_busy);
            return;
        }
        if self.status != ProxyStatus::Running {
            self.notify(self.text().msg_proxy_not_running);
            return;
        }
        self.restart_confirm = true;
        self.message = None;
    }

    fn perform_restart(&mut self) {
        if self.setup.downloading || self.setup.rollback_in_flight || self.install_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        if matches!(self.status, ProxyStatus::Starting | ProxyStatus::Stopping) {
            self.notify(self.text().msg_proxy_busy);
            return;
        }
        if self.status != ProxyStatus::Running || self.proxy.is_none() {
            self.notify(self.text().msg_proxy_not_running);
            return;
        }
        self.begin_restart();
    }

    fn auto_restart_after_change(&mut self) -> bool {
        if self.restart_in_progress {
            return false;
        }
        if self.status != ProxyStatus::Running || self.proxy.is_none() {
            return false;
        }
        if self.pending_restart_subscription.is_some() {
            return false;
        }
        if self.setup.downloading || self.setup.rollback_in_flight || self.install_in_flight {
            return false;
        }
        if self.nodes.is_empty() {
            self.last_error = Some(self.text().no_nodes.to_string());
            self.notify(self.text().no_nodes);
            return false;
        }
        let filtered = crate::selection::filter_node_tags(
            &self.nodes,
            &self.config.keywords_include,
            &self.config.keywords_exclude,
        );
        if filtered.is_empty() {
            self.last_error = Some(self.text().no_matching_nodes.to_string());
            self.notify(self.text().no_matching_nodes);
            return false;
        }
        self.begin_restart()
    }

    fn begin_restart(&mut self) -> bool {
        // Validate the proposed configuration before tearing down the running
        // instance. `start` repeats this check after stopping (it is the final
        // safety net), but a failed proposal must not cause an outage.
        if let Err(err) = supervisor::preflight(
            &self.paths,
            &self.config,
            &self.nodes,
            self.preferred_node.as_deref(),
        ) {
            let message = format!("{}: {err}", self.text().msg_preflight_failed);
            self.last_error = Some(message.clone());
            self.logger.app(LogLevel::Warn, message);
            self.notify(self.text().msg_preflight_failed);
            self.restart_confirm = false;
            self.rollback_confirm = false;
            return false;
        }
        self.restart_confirm = false;
        self.rollback_confirm = false;
        self.subscription_delete = None;
        self.restart_in_progress = true;
        self.restart_after_stop = true;
        self.notify(self.text().msg_restarting);
        self.stop_proxy();
        true
    }

    fn start_proxy(&mut self) -> Result<(), String> {
        if matches!(self.status, ProxyStatus::Starting | ProxyStatus::Stopping) {
            return Err(self.text().msg_proxy_busy.to_string());
        }
        if self.binary_manager.locate().is_none() {
            self.open_setup();
            self.last_error = Some(self.text().msg_singbox_missing.to_string());
            return Err(self.text().msg_singbox_missing.to_string());
        }
        if self.nodes.is_empty() {
            self.last_error = Some(self.text().no_nodes.to_string());
            self.switch_page(Page::Subscriptions);
            return Err(self.text().no_nodes.to_string());
        }
        let filtered = crate::selection::filter_node_tags(
            &self.nodes,
            &self.config.keywords_include,
            &self.config.keywords_exclude,
        );
        if filtered.is_empty() {
            self.last_error = Some(self.text().no_matching_nodes.to_string());
            return Err(self.text().no_matching_nodes.to_string());
        }
        self.runtime_has_auto = filtered.len() >= 2;
        let mut preferred = self.preferred_node.clone();
        if let Some(tag) = preferred.as_deref() {
            if !self.nodes.iter().any(|node| node.tag == tag) {
                preferred = None;
                self.preferred_node = None;
                self.pending_selection = None;
                self.notify(self.text().msg_preferred_missing);
            }
        }
        self.proxy_node_tags = self.nodes.iter().map(|node| node.tag.clone()).collect();
        self.proxy_node_names = self
            .nodes
            .iter()
            .map(|node| (node.tag.clone(), node.name.clone()))
            .collect();
        self.status = ProxyStatus::Starting;
        let paths = self.paths.clone();
        let config = self.config.clone();
        let nodes = self.nodes.clone();
        let logger = self.logger.clone();
        let tx = self.events_tx.clone();
        tokio::spawn(async move {
            let result = supervisor::start(&paths, &config, &nodes, &logger, preferred.as_deref())
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::ProxyStarted(result));
        });
        Ok(())
    }

    fn schedule_restart(&mut self) {
        if self.restart_attempts >= MAX_RESTART_ATTEMPTS {
            self.restart_at = None;
            self.notify(self.text().msg_restart_paused);
            self.logger
                .app(LogLevel::Error, "restart paused after 5 failures");
            return;
        }
        let delay = RESTART_DELAYS[self.restart_attempts as usize];
        self.restart_attempts += 1;
        self.restart_at = Some(Instant::now() + Duration::from_secs(delay));
        self.notify(format!(
            "{}: {}s (#{})",
            self.text().msg_restart_scheduled,
            delay,
            self.restart_attempts
        ));
        self.logger.app(
            LogLevel::Warn,
            format!(
                "restart scheduled in {delay}s (attempt {}/{MAX_RESTART_ATTEMPTS})",
                self.restart_attempts
            ),
        );
    }

    fn stop_proxy(&mut self) {
        if let Some(task) = self.metrics_task.take() {
            task.abort();
        }
        self.restart_at = None;
        self.restart_attempts = 0;
        self.stable_reset_at = None;
        self.reset_metrics();
        if let Some(proxy) = self.proxy.take() {
            self.status = ProxyStatus::Stopping;
            let logger = self.logger.clone();
            let tx = self.events_tx.clone();
            tokio::spawn(async move {
                supervisor::stop(proxy, &logger).await;
                let _ = tx.send(AppEvent::ProxyStopped);
            });
        } else {
            self.status = ProxyStatus::Stopped;
        }
    }

    fn start_metrics(&mut self, port: u16, secret: String) {
        if let Some(task) = self.metrics_task.take() {
            task.abort();
        }
        let tx = self.events_tx.clone();
        let client = ClashClient::new(port, secret);
        self.logger.app(
            LogLevel::Info,
            format!("{}: {}", self.text().msg_clash_api, client.base_url()),
        );
        let clash_api_label = self.text().msg_clash_api;
        let viewer_active = self.viewer_active.clone();
        let metrics_wake = self.metrics_wake.clone();
        let proxies_refresh = self.proxies_refresh.clone();
        self.metrics_task = Some(tokio::spawn(async move {
            let mut selection = ProxySelection::default();
            let mut node_delays = HashMap::new();
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
                        report_clash_error(&tx, clash_api_label, &mut last_connection_error, err);
                        None
                    }
                };
                if Instant::now() >= next_proxies || proxies_refresh.swap(false, Ordering::Relaxed)
                {
                    match client.proxies().await {
                        Ok(snapshot) => {
                            selection = snapshot.selection;
                            node_delays = snapshot.node_delays;
                            last_proxy_error = None;
                        }
                        Err(err) => {
                            report_clash_error(&tx, clash_api_label, &mut last_proxy_error, err);
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
                        node_delays: node_delays.clone(),
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
        }));
    }

    fn refresh_proxy_ips(&mut self) {
        if self.proxy.is_none() || self.proxy_ips_in_flight {
            return;
        }
        self.proxy_ips_in_flight = true;
        self.last_proxy_ips = Instant::now();
        let tx = self.events_tx.clone();
        let logger = self.logger.clone();
        let v4_url = self.config.proxy_ip_v4_url.clone();
        let v6_url = self.config.proxy_ip_v6_url.clone();
        let proxy_url = match self
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.lan_auth.as_ref())
        {
            Some((username, password)) => netinfo::local_proxy_url_with_auth(
                self.proxy
                    .as_ref()
                    .map_or(self.config.port, |proxy| proxy.port),
                username,
                password,
            ),
            None => netinfo::local_proxy_url(
                self.proxy
                    .as_ref()
                    .map_or(self.config.port, |proxy| proxy.port),
            ),
        };
        tokio::spawn(async move {
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
            if ips.is_empty() {
                logger.app(
                    LogLevel::Warn,
                    format!("proxy IP lookup failed: {}", errors.join("; ")),
                );
            }
            let _ = tx.send(AppEvent::ProxyIps(ips));
        });
    }

    fn request_rollback(&mut self) {
        if self.setup.downloading || self.setup.rollback_in_flight || self.install_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        if !self.paths.previous_binary().is_file() {
            self.notify(self.text().msg_rollback_unavailable);
            return;
        }
        self.rollback_confirm = true;
        self.message = None;
    }

    fn perform_rollback(&mut self) {
        if self.setup.downloading || self.setup.rollback_in_flight || self.install_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        if !self.paths.previous_binary().is_file() {
            self.notify(self.text().msg_rollback_unavailable);
            return;
        }
        self.setup.rollback_in_flight = true;
        self.setup.message = None;
        let manager = self.binary_manager.clone();
        let logger = self.logger.clone();
        let tx = self.events_tx.clone();
        tokio::spawn(async move {
            let result = manager
                .rollback(&logger)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::RollbackDone(result));
        });
    }

    fn start_download(&mut self) {
        if self.setup.downloading || self.install_in_flight {
            return;
        }
        if self.setup.rollback_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        self.setup.downloading = true;
        self.setup.message = None;
        let config = self.config.clone();
        let logger = self.logger.clone();
        let manager = self.binary_manager.clone();
        let tx = self.events_tx.clone();
        tokio::spawn(async move {
            let result = manager
                .download_latest(&config, &logger)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::DownloadDone(result));
        });
    }

    async fn install_selected(&mut self) {
        if self.setup.downloading || self.install_in_flight {
            return;
        }
        if self.setup.rollback_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        let Some(candidate) = self.setup.candidates.get(self.selected).cloned() else {
            return;
        };
        if !candidate.has_checksum && self.setup.pending_install.as_ref() != Some(&candidate.path) {
            self.setup.pending_install = Some(candidate.path.clone());
            self.setup.message = None;
            return;
        }
        self.setup.pending_install = None;
        self.install_in_flight = true;
        self.setup.installing = true;
        self.setup.message = None;
        let manager = self.binary_manager.clone();
        let logger = self.logger.clone();
        let tx = self.events_tx.clone();
        let path = candidate.path.clone();
        tokio::spawn(async move {
            let result = manager
                .install_from_inbox(&path, &logger)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::InstallDone(result));
        });
    }

    async fn select_node(&mut self) {
        let Some(node) = self.nodes.get(self.selected).cloned() else {
            return;
        };
        if matches!(self.status, ProxyStatus::Starting | ProxyStatus::Stopping) {
            self.last_error = Some(self.text().msg_proxy_busy.to_string());
            return;
        }
        if self.proxy.is_none() {
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
        if !self.proxy_node_tags.is_empty() && !self.proxy_node_tags.contains(&node.tag) {
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
        tokio::spawn(async move {
            match client.select(&tag).await {
                Ok(()) => {
                    let _ = tx.send(AppEvent::NodeSelected(tag));
                    let _ = tx.send(AppEvent::Info(format!("{selected_label}: {name}")));
                    let _ = tx.send(AppEvent::SelectionApplied);
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
                if target == "auto" && !self.runtime_has_auto {
                    target = crate::selection::filter_node_tags(
                        &self.nodes,
                        &self.config.keywords_include,
                        &self.config.keywords_exclude,
                    )
                    .first()
                    .cloned()
                    .unwrap_or(target);
                }
                (target, self.text().msg_auto_enabled.to_string())
            }
            PendingSelection::Node(tag) => {
                let name = self
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
        tokio::spawn(async move {
            let client = ClashClient::new(port, secret);
            for attempt in 0..20 {
                if token.load(Ordering::Relaxed) != generation {
                    return;
                }
                match client.select(&target).await {
                    Ok(()) => {
                        let _ = tx.send(AppEvent::Info(label));
                        let _ = tx.send(AppEvent::SelectionApplied);
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
        let Some(node) = self.nodes.get(self.selected) else {
            return;
        };
        let Some(proxy) = self.proxy.as_ref() else {
            self.last_error = Some(self.text().msg_proxy_not_running.to_string());
            return;
        };
        let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
        let tag = node.tag.clone();
        self.delay_testing.insert(tag.clone());
        let test_url = self.config.test_url.clone();
        let label = self.text().msg_node_delay;
        let tx = self.events_tx.clone();
        tokio::spawn(async move {
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
        if self.proxy.is_none() || self.delay_sweep_running {
            return;
        }
        if !force
            && self
                .last_delay_sweep
                .is_some_and(|at| at.elapsed() < DELAY_SWEEP_COOLDOWN)
        {
            return;
        }
        let Some(proxy) = self.proxy.as_ref() else {
            return;
        };
        let tags: Vec<String> = self
            .nodes
            .iter()
            .take(MAX_LIST_ROWS)
            .filter(|node| force || !self.node_delays.contains_key(&node.tag))
            .map(|node| node.tag.clone())
            .collect();
        if tags.is_empty() {
            return;
        }
        let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
        let test_url = self.config.test_url.clone();
        let tx = self.events_tx.clone();
        self.delay_sweep_running = true;
        self.last_delay_sweep = Some(Instant::now());
        self.delay_testing = tags.iter().cloned().collect();
        tokio::spawn(async move {
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

    fn health_target(&self) -> Option<String> {
        let selected = self.metrics.selection.selected.as_deref()?;
        let tag = if selected == "auto" {
            self.metrics
                .selection
                .auto_now
                .as_deref()
                .filter(|tag| *tag != "auto")?
        } else {
            selected
        };
        self.proxy_node_tags
            .iter()
            .any(|item| item == tag)
            .then(|| tag.to_string())
    }

    fn maybe_probe_health(&mut self) {
        if self.proxy.is_none()
            || self.status != ProxyStatus::Running
            || self.recovery_in_flight
            || self.recovery_refresh_pending
            || self.health_probe_in_flight
            || self
                .health_pause_until
                .is_some_and(|at| at > Instant::now())
        {
            return;
        }
        let now = Instant::now();
        if self
            .last_metrics_at
            .is_none_or(|at| at.elapsed() > HEALTH_PROBE_STALE)
        {
            return;
        }
        if self
            .last_health_activity
            .is_none_or(|at| at.elapsed() > HEALTH_ACTIVE_WINDOW)
        {
            return;
        }
        if self
            .last_health_probe
            .is_some_and(|at| at.elapsed() < HEALTH_PROBE_INTERVAL)
        {
            return;
        }
        let Some(tag) = self.health_target() else {
            return;
        };
        let Some(proxy) = self.proxy.as_ref() else {
            return;
        };
        let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
        let test_url = self.config.test_url.clone();
        let tx = self.events_tx.clone();
        let logger = self.logger.clone();
        let generation = self.health_generation;
        self.health_probe_in_flight = true;
        self.last_health_probe = Some(now);
        tokio::spawn(async move {
            let result = client.delay(&tag, &test_url).await;
            let outcome = health_outcome(&result);
            if matches!(outcome, HealthOutcome::Unavailable) {
                if let Err(err) = &result {
                    logger.app(
                        LogLevel::Debug,
                        format!("health probe unavailable for {tag}: {err}"),
                    );
                }
            }
            let _ = tx.send(AppEvent::HealthProbe {
                generation,
                tag,
                outcome,
            });
        });
    }

    fn reset_health_failures(&mut self) {
        self.health_failures = 0;
        self.health_failure_tag = None;
    }

    fn invalidate_health_tasks(&mut self) {
        self.health_generation = self.health_generation.wrapping_add(1);
        self.health_probe_in_flight = false;
        self.recovery_in_flight = false;
        self.health_pause_until = None;
        self.recovery_refresh_pending = false;
        self.reset_health_failures();
        self.last_recovery = None;
    }

    fn reset_health_state(&mut self) {
        self.invalidate_health_tasks();
        self.last_health_probe = None;
        self.last_health_activity = None;
        self.health_connection_ids.clear();
        self.last_metrics_at = None;
        self.recovery_refresh_pending = false;
    }

    fn recovery_candidates(&self, trigger: &str) -> Vec<String> {
        let runtime: HashSet<&str> = self.proxy_node_tags.iter().map(String::as_str).collect();
        let auto_mode = self.metrics.selection.selected.as_deref() == Some("auto");
        let auto_nodes = if auto_mode {
            crate::selection::filter_nodes(
                &self.nodes,
                &self.config.keywords_include,
                &self.config.keywords_exclude,
            )
        } else {
            Vec::new()
        };
        let source: Vec<&Node> = if auto_mode {
            auto_nodes
                .iter()
                .filter(|node| runtime.contains(node.tag.as_str()))
                .collect()
        } else {
            self.nodes
                .iter()
                .filter(|node| runtime.contains(node.tag.as_str()))
                .collect()
        };
        let mut ordered: Vec<&Node> = source
            .into_iter()
            .filter(|node| node.tag != trigger)
            .collect();
        ordered.sort_by(|left, right| {
            self.node_delays
                .get(&left.tag)
                .copied()
                .unwrap_or(u64::MAX)
                .cmp(
                    &self
                        .node_delays
                        .get(&right.tag)
                        .copied()
                        .unwrap_or(u64::MAX),
                )
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.tag.cmp(&right.tag))
        });
        ordered.truncate(MAX_LIST_ROWS);
        ordered.into_iter().map(|node| node.tag.clone()).collect()
    }

    fn start_recovery(&mut self, trigger: String) {
        if self.recovery_in_flight
            || self.recovery_refresh_pending
            || self
                .health_pause_until
                .is_some_and(|at| at > Instant::now())
        {
            return;
        }
        let Some(proxy) = self.proxy.as_ref() else {
            return;
        };
        self.last_recovery = Some(Instant::now());
        self.logger.app(
            LogLevel::Warn,
            format!("health failover: {trigger} failed {HEALTH_FAILURE_THRESHOLD} probes"),
        );
        let candidates = self.recovery_candidates(&trigger);
        if candidates.is_empty() {
            self.on_no_healthy_nodes();
            return;
        }
        let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
        let test_url = self.config.test_url.clone();
        let tx = self.events_tx.clone();
        let generation = self.health_generation;
        self.recovery_in_flight = true;
        self.notify(self.text().msg_health_recovering);
        tokio::spawn(async move {
            let mut healthy = Vec::new();
            for chunk in candidates.chunks(DELAY_TEST_CONCURRENCY) {
                let mut tasks = JoinSet::new();
                for tag in chunk.iter().cloned() {
                    let client = client.clone();
                    let test_url = test_url.clone();
                    tasks.spawn(async move {
                        let delay = client
                            .delay(&tag, &test_url)
                            .await
                            .ok()
                            .filter(|delay| *delay > 0);
                        (tag, delay)
                    });
                }
                while let Some(result) = tasks.join_next().await {
                    if let Ok((tag, delay)) = result {
                        if let Some(delay) = delay {
                            healthy.push((tag.clone(), delay));
                        }
                        let _ = tx.send(AppEvent::NodeDelay { tag, delay });
                    }
                }
                if !healthy.is_empty() {
                    break;
                }
            }
            let _ = tx.send(AppEvent::RecoveryProbe {
                generation,
                trigger,
                healthy,
            });
        });
    }

    fn on_no_healthy_nodes(&mut self) {
        self.recovery_in_flight = false;
        self.reset_health_failures();
        self.health_pause_until = Some(Instant::now() + HEALTH_NO_NODE_PAUSE);
        let message = self.text().msg_health_no_nodes.to_string();
        self.last_error = Some(message.clone());
        self.logger
            .app(LogLevel::Warn, "health failover: no healthy nodes");
        if self.config.enabled_subscriptions().next().is_none() {
            self.notify(self.text().msg_no_enabled_subscriptions);
            return;
        }
        if self
            .last_recovery_refresh
            .is_some_and(|at| at.elapsed() < HEALTH_REFRESH_COOLDOWN)
        {
            self.notify(message);
            return;
        }
        if self.refreshing_subscriptions {
            self.recovery_refresh_pending = true;
            return;
        }
        self.last_recovery_refresh = Some(Instant::now());
        self.recovery_refresh_pending = true;
        self.notify(self.text().msg_health_refreshing);
        self.refresh_subscriptions();
    }

    fn finish_recovery_refresh(&mut self) {
        self.reset_health_failures();
        let auto_nodes = crate::selection::filter_nodes(
            &self.nodes,
            &self.config.keywords_include,
            &self.config.keywords_exclude,
        );
        let has_new = auto_nodes
            .iter()
            .any(|node| !self.proxy_node_tags.contains(&node.tag));
        if !has_new {
            self.last_error = Some(self.text().msg_health_no_nodes.to_string());
            self.health_pause_until = Some(Instant::now() + HEALTH_NO_NODE_PAUSE);
            self.logger.app(
                LogLevel::Warn,
                "health failover: subscription refresh added no auto candidates",
            );
            self.notify(self.text().msg_health_no_new_nodes);
            return;
        }
        if self.proxy.is_some() && self.status == ProxyStatus::Running {
            self.preferred_node = None;
            self.pending_selection = None;
            self.perform_restart();
            self.notify(self.text().msg_health_restarting);
        } else {
            self.notify(self.text().msg_health_new_nodes);
        }
    }

    fn auto_selection_target(&self) -> Option<String> {
        let filtered = crate::selection::filter_node_tags(
            &self.nodes,
            &self.config.keywords_include,
            &self.config.keywords_exclude,
        );
        if filtered.len() >= 2 {
            Some("auto".to_string())
        } else {
            filtered.first().cloned()
        }
    }

    fn select_node_auto(&mut self) {
        if matches!(self.status, ProxyStatus::Starting | ProxyStatus::Stopping) {
            self.last_error = Some(self.text().msg_proxy_busy.to_string());
            return;
        }
        if self.proxy.is_none() {
            self.preferred_node = None;
            self.pending_selection = Some(PendingSelection::Auto);
            self.notify(self.text().msg_auto_pending);
            return;
        }
        self.selection_generation.fetch_add(1, Ordering::Relaxed);
        let Some(target) = self.auto_selection_target() else {
            self.last_error = Some(self.text().no_matching_nodes.to_string());
            return;
        };
        // A running config built with fewer than two auto candidates has no
        // urltest group. If a refresh added candidates, restart to regenerate
        // the topology instead of sending a selector request for a missing
        // "auto" child.
        if target == "auto" && !self.runtime_has_auto {
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
        let tx = self.events_tx.clone();
        let auto_label = self.text().msg_auto_enabled;
        tokio::spawn(async move {
            match client.select(&target).await {
                Ok(()) => {
                    let _ = tx.send(AppEvent::Info(auto_label.to_string()));
                    let _ = tx.send(AppEvent::SelectionApplied);
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::Error(err.to_string()));
                }
            }
        });
    }

    fn reset_metrics(&mut self) {
        self.up_speed = 0.0;
        self.down_speed = 0.0;
        self.last_totals = None;
        self.metrics = MetricsSnapshot::default();
        self.proxy_ips.clear();
        self.sort_nodes_if_visible();
        self.proxy_ips_in_flight = false;
        self.proxy_ip_backoff_secs = 5;
        self.node_delays.clear();
        self.delay_testing.clear();
        self.delay_sweep_running = false;
        self.last_delay_sweep = None;
        self.reset_health_state();
    }

    fn toggle_language(&mut self) {
        let mut config = self.config.clone();
        config.language = config.language.toggle();
        if let Err(err) = config.save(&self.paths.config_file) {
            self.last_error = Some(err.to_string());
        } else {
            self.config = config;
            self.notify(self.text().settings_saved);
        }
    }

    pub fn toggle_allow_lan(&mut self) {
        let mut config = self.config.clone();
        config.allow_lan = !config.allow_lan;
        if let Err(err) = config.save(&self.paths.config_file) {
            self.last_error = Some(err.to_string());
            return;
        }
        self.config = config;
        self.selected = self.selected.min(
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
        if self.message_at.is_some_and(|at| at.elapsed() >= TOAST_TTL) {
            self.message = None;
            self.message_at = None;
            changed = true;
        }
        let mut crashed = None;
        if let Some(proxy) = self.proxy.as_mut() {
            if let Ok(Some(status)) = proxy.child.try_wait() {
                crashed = Some(status);
            }
        }
        if let Some(status) = crashed {
            self.proxy = None;
            if let Some(task) = self.metrics_task.take() {
                task.abort();
            }
            self.status = ProxyStatus::Failed;
            self.runtime_has_auto = false;
            self.stable_reset_at = None;
            self.proxy_node_tags.clear();
            self.proxy_node_names.clear();
            self.last_error = Some(format!("{}: {status}", self.text().msg_singbox_exited));
            self.started_at = None;
            self.reset_metrics();
            self.logger.app(
                LogLevel::Error,
                format!("sing-box exited unexpectedly: {status}"),
            );
            self.schedule_restart();
            changed = true;
        }
        if self.proxy.is_some() && self.stable_reset_at.is_some_and(|at| Instant::now() >= at) {
            self.restart_attempts = 0;
            self.stable_reset_at = None;
        }
        if self.has_viewer() && self.page == Page::Logs && self.log_follow {
            self.selected = self.log_count().saturating_sub(1);
        }
        if self.last_local_ips.elapsed() >= Duration::from_secs(30) {
            self.local_ips = netinfo::local_addresses(false);
            self.last_local_ips = Instant::now();
            changed = true;
        }
        if self.proxy.is_some() && !self.proxy_ips_in_flight {
            let interval = if self.proxy_ips.is_empty() {
                Duration::from_secs(self.proxy_ip_backoff_secs)
            } else {
                Duration::from_secs(300)
            };
            if self.last_proxy_ips.elapsed() >= interval {
                self.refresh_proxy_ips();
            }
        }
        self.maybe_probe_health();
        if self.config.subscription_refresh_hours > 0
            && self.config.enabled_subscriptions().next().is_some()
            && self.last_subscription_refresh.is_none_or(|last| {
                last.elapsed()
                    >= Duration::from_secs(
                        self.config.subscription_refresh_hours.saturating_mul(3600),
                    )
            })
        {
            self.refresh_subscriptions();
            changed = true;
        }
        if self.status == ProxyStatus::Failed
            && self.proxy.is_none()
            && self.restart_at.is_some_and(|at| Instant::now() >= at)
        {
            self.restart_at = None;
            let _ = self.start_proxy();
            changed = true;
        }
        self.logger.flush();
        changed
    }

    pub fn current_node(&self) -> Option<String> {
        let active = self.metrics.selection.selected.as_deref().map(|selected| {
            if selected == "auto" {
                self.metrics
                    .selection
                    .auto_now
                    .clone()
                    .unwrap_or_else(|| "auto".to_string())
            } else {
                selected.to_string()
            }
        });
        let fallback = self.preferred_node.clone();
        match active.or(fallback) {
            Some(tag) if tag == "auto" => Some(self.text().common_auto.to_string()),
            Some(tag) => Some(self.node_display_name(&tag)),
            None if !self.nodes.is_empty() => Some(self.text().common_auto.to_string()),
            None => None,
        }
    }

    /// Whether nodes are picked automatically (urltest) or pinned manually.
    ///
    /// While running the selector state is authoritative; before startup the
    /// pending preferred node decides the mode.
    pub fn selection_mode_auto(&self) -> bool {
        match self.metrics.selection.selected.as_deref() {
            Some(selected) => selected == "auto",
            None => self.preferred_node.is_none(),
        }
    }

    fn node_display_name(&self, tag: &str) -> String {
        if let Some(node) = self.nodes.iter().find(|node| node.tag == tag) {
            return sanitize_display(&node.name);
        }
        if let Some(name) = self.proxy_node_names.get(tag) {
            return format!(
                "{}{}",
                sanitize_display(name),
                self.text().node_removed_suffix
            );
        }
        self.text().node_unknown.to_string()
    }

    pub fn proxy_pid(&self) -> u32 {
        self.proxy.as_ref().map(|proxy| proxy.pid).unwrap_or(0)
    }

    pub fn uptime(&self) -> String {
        let Some(started) = self.started_at else {
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
        let (v4, port, ipv6) = match self.proxy.as_ref() {
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

fn sort_nodes(
    nodes: &mut [Node],
    mode: NodeSort,
    auto_tags: &HashSet<String>,
    delays: &HashMap<String, u64>,
) {
    match mode {
        NodeSort::AutoLatency => nodes.sort_by(|left, right| {
            (!auto_tags.contains(&left.tag))
                .cmp(&(!auto_tags.contains(&right.tag)))
                .then_with(|| {
                    delays
                        .get(&left.tag)
                        .copied()
                        .unwrap_or(u64::MAX)
                        .cmp(&delays.get(&right.tag).copied().unwrap_or(u64::MAX))
                })
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.tag.cmp(&right.tag))
        }),
        NodeSort::Name => nodes.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.tag.cmp(&right.tag))
        }),
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
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;
    use tempfile::TempDir;

    async fn test_app() -> (App, TempDir) {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        paths.ensure().unwrap();
        let config = Config::default();
        let logger = Logger::new(&paths.logs_dir, LogLevel::Info, LogLevel::Info).unwrap();
        (App::new(config, paths, logger).await, dir)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn setting_index(app: &App, item: SettingsItem) -> usize {
        settings_items(app.config.allow_lan)
            .iter()
            .position(|entry| *entry == item)
            .unwrap()
    }

    fn node(name: &str) -> Node {
        Node::new(
            name.to_string(),
            "test".to_string(),
            "shadowsocks".to_string(),
            "example.com".to_string(),
            443,
            serde_json::json!({
                "type": "shadowsocks", "server": "example.com", "server_port": 443,
                "method": "aes-128-gcm", "password": "secret"
            }),
        )
    }

    #[tokio::test]
    async fn hjkl_navigates_pages_and_lists() {
        let (mut app, _dir) = test_app().await;
        app.nodes = vec![node("one"), node("two"), node("three")];
        app.page = Page::Nodes;

        app.handle_key(key(KeyCode::Char('j'))).await;
        assert_eq!(app.selected, 1);
        app.handle_key(key(KeyCode::Char('k'))).await;
        assert_eq!(app.selected, 0);
        app.handle_key(key(KeyCode::Char('G'))).await;
        assert_eq!(app.selected, 2);
        app.handle_key(key(KeyCode::Char('g'))).await;
        assert_eq!(app.selected, 0);

        app.handle_key(key(KeyCode::Char('l'))).await;
        assert_eq!(app.page, Page::Subscriptions);
        app.handle_key(key(KeyCode::Char('h'))).await;
        assert_eq!(app.page, Page::Nodes);
        app.handle_key(key(KeyCode::Right)).await;
        assert_eq!(app.page, Page::Subscriptions);
        app.handle_key(key(KeyCode::Left)).await;
        assert_eq!(app.page, Page::Nodes);
    }

    #[tokio::test]
    async fn home_and_end_jump_like_g_and_big_g() {
        let (mut app, _dir) = test_app().await;
        app.nodes = vec![node("one"), node("two"), node("three")];
        app.page = Page::Nodes;
        app.handle_key(key(KeyCode::End)).await;
        assert_eq!(app.selected, 2);
        app.handle_key(key(KeyCode::Home)).await;
        assert_eq!(app.selected, 0);

        app.page = Page::Dashboard;
        app.dashboard_max_scroll = 4;
        app.handle_key(key(KeyCode::End)).await;
        assert_eq!(app.dashboard_scroll, 4);
        app.handle_key(key(KeyCode::Home)).await;
        assert_eq!(app.dashboard_scroll, 0);

        app.page = Page::Help;
        app.help_max_scroll = 3;
        app.handle_key(key(KeyCode::End)).await;
        assert_eq!(app.help_scroll, 3);
        app.handle_key(key(KeyCode::Home)).await;
        assert_eq!(app.help_scroll, 0);
    }

    #[tokio::test]
    async fn confirmation_keys_only_cancel() {
        let (mut app, _dir) = test_app().await;
        app.nodes = vec![node("one"), node("two")];
        app.page = Page::Nodes;
        app.selected = 0;

        app.status = ProxyStatus::Running;
        app.handle_key(key(KeyCode::Char('R'))).await;
        assert!(app.restart_confirm);
        app.handle_key(key(KeyCode::Char('j'))).await;
        assert!(!app.restart_confirm);
        assert_eq!(app.selected, 0);

        app.page = Page::Subscriptions;
        let config = SubscriptionConfig {
            id: "s-1".to_string(),
            name: "test".to_string(),
            url: "https://example.com/sub".to_string(),
            enabled: false,
        };
        app.config.subscriptions.push(config.clone());
        app.subscriptions.push(empty_runtime(config));
        app.selected = 0;
        app.handle_key(key(KeyCode::Char('d'))).await;
        assert!(app.subscription_delete.is_some());
        app.handle_key(key(KeyCode::Char('n'))).await;
        assert!(app.subscription_delete.is_none());
        assert!(app.overlay.is_none());
    }

    #[tokio::test]
    async fn ctrl_c_clears_other_confirmations() {
        let (mut app, _dir) = test_app().await;
        app.restart_confirm = true;
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .await;
        assert!(!app.restart_confirm);
        assert!(!app.should_quit);

        app.subscription_delete = Some("s-1".to_string());
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .await;
        assert!(app.subscription_delete.is_none());
        assert!(!app.should_quit);
    }

    #[tokio::test]
    async fn start_stop_reports_busy_state() {
        let (mut app, _dir) = test_app().await;
        app.status = ProxyStatus::Starting;
        app.handle_key(key(KeyCode::Char('s'))).await;
        assert_eq!(app.message.as_deref(), Some(app.text().msg_proxy_busy));
    }

    #[tokio::test]
    async fn dashboard_enter_opens_logs_only_when_an_error_is_shown() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Dashboard;
        app.handle_key(key(KeyCode::Enter)).await;
        assert_eq!(app.page, Page::Dashboard);

        app.last_error = Some("boom".to_string());
        app.handle_key(key(KeyCode::Enter)).await;
        assert_eq!(app.page, Page::Logs);
    }

    #[tokio::test]
    async fn settings_install_entry_opens_setup_and_returns_to_settings() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Settings;
        app.selected = setting_index(&app, SettingsItem::BinaryInfo);
        app.handle_key(key(KeyCode::Enter)).await;
        assert_eq!(app.page, Page::Setup);
        assert_eq!(app.setup_return, Some(Page::Settings));
        assert_eq!(app.view().tab_page, Page::Settings);

        app.handle_key(key(KeyCode::Char('?'))).await;
        assert_eq!(app.page, Page::Help);
        assert_eq!(app.view().tab_page, Page::Settings);
        app.handle_key(key(KeyCode::Esc)).await;
        assert_eq!(app.page, Page::Setup);
        assert_eq!(app.view().tab_page, Page::Settings);

        app.handle_key(key(KeyCode::Esc)).await;
        assert_eq!(app.page, Page::Settings);
        assert_eq!(app.setup_return, None);
        assert_eq!(app.view().tab_page, Page::Settings);
    }

    #[tokio::test]
    async fn binary_status_tracks_install_and_upgrade() {
        let (mut app, _dir) = test_app().await;
        assert_eq!(app.view().binary_status, BinaryStatus::NotInstalled);

        app.binary_installed = true;
        app.binary_version = Some("1.14.4".to_string());
        assert_eq!(app.view().binary_status, BinaryStatus::Installed);

        app.latest_binary_version = Some("1.15.0".to_string());
        assert_eq!(app.view().binary_status, BinaryStatus::UpgradeAvailable);

        app.latest_binary_version = Some("1.14.4".to_string());
        assert_eq!(app.view().binary_status, BinaryStatus::Installed);

        app.latest_binary_version = Some("1.14.4-alpha.13".to_string());
        assert_eq!(app.view().binary_status, BinaryStatus::Installed);

        app.handle_event(AppEvent::UpdateCheckDone(Ok("1.16.0".to_string())))
            .await;
        assert!(!app.update_check_in_flight);
        assert_eq!(app.latest_binary_version.as_deref(), Some("1.16.0"));
        assert_eq!(app.view().binary_status, BinaryStatus::UpgradeAvailable);

        app.handle_event(AppEvent::UpdateCheckDone(Err("offline".to_string())))
            .await;
        assert!(!app.update_check_in_flight);
        assert_eq!(app.latest_binary_version.as_deref(), Some("1.16.0"));
    }

    #[tokio::test]
    async fn rollback_requires_previous_binary() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Setup;
        app.handle_key(key(KeyCode::Char('b'))).await;
        assert_eq!(
            app.message.as_deref(),
            Some(app.text().msg_rollback_unavailable)
        );
        assert!(!app.rollback_confirm);
    }

    #[tokio::test]
    async fn rollback_confirmation_is_consumed_by_other_keys() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Setup;
        std::fs::write(app.paths.previous_binary(), b"previous").unwrap();
        app.handle_key(key(KeyCode::Char('b'))).await;
        assert!(app.rollback_confirm);
        app.handle_key(key(KeyCode::Char('j'))).await;
        assert!(!app.rollback_confirm);
    }

    #[tokio::test]
    async fn unverified_install_confirmation_is_cancelled_by_other_keys() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Setup;
        app.setup.candidates = vec![InboxCandidate {
            path: PathBuf::from("/inbox/raw-sing-box"),
            label: "raw-sing-box".to_string(),
            has_checksum: false,
        }];
        let expected = PathBuf::from("/inbox/raw-sing-box");
        app.handle_key(key(KeyCode::Enter)).await;
        assert_eq!(app.setup.pending_install.as_ref(), Some(&expected));
        assert!(app.setup.message.is_none());
        assert!(!app.should_quit);

        app.handle_key(key(KeyCode::Char('j'))).await;
        assert!(app.setup.pending_install.is_none());
        assert_eq!(app.page, Page::Setup);

        app.handle_key(key(KeyCode::Enter)).await;
        assert!(app.setup.pending_install.is_some());
        app.handle_key(key(KeyCode::Char('q'))).await;
        assert!(app.setup.pending_install.is_none());
        assert!(!app.should_quit);

        app.handle_key(key(KeyCode::Enter)).await;
        assert!(app.setup.pending_install.is_some());
        app.handle_key(key(KeyCode::Esc)).await;
        assert!(app.setup.pending_install.is_none());
        assert_eq!(app.page, Page::Setup);
    }

    #[tokio::test]
    async fn ctrl_c_clears_unverified_install_confirmation() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Setup;
        app.setup.pending_install = Some(PathBuf::from("/inbox/raw-sing-box"));
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .await;
        assert!(app.setup.pending_install.is_none());
        assert!(!app.should_quit);
    }

    #[tokio::test]
    async fn rollback_done_updates_the_current_version() {
        let (mut app, _dir) = test_app().await;
        app.binary_installed = true;
        app.binary_version = Some("1.15.0".to_string());
        app.previous_binary_available = true;
        app.previous_binary_version = Some("1.14.4".to_string());
        app.setup.rollback_in_flight = true;
        let metadata = BinaryMetadata {
            version: "1.14.4".to_string(),
            source: "test".to_string(),
            sha256: None,
            installed_at: "2026-11-01 00:00:00".to_string(),
            verified: true,
        };
        app.handle_event(AppEvent::RollbackDone(Ok(metadata))).await;
        assert_eq!(app.binary_version.as_deref(), Some("1.14.4"));
        assert!(!app.setup.rollback_in_flight);
        assert!(app
            .message
            .as_deref()
            .unwrap_or_default()
            .contains(app.text().msg_binary_switched));
    }

    #[tokio::test]
    async fn subscription_refresh_clears_pending_auto_restart() {
        let (mut app, _dir) = test_app().await;
        let config = SubscriptionConfig {
            id: "s-1".to_string(),
            name: "test".to_string(),
            url: "https://example.com/sub".to_string(),
            enabled: true,
        };
        app.config.subscriptions.push(config.clone());
        app.subscriptions.push(empty_runtime(config.clone()));
        app.subscription_refresh_seq.insert(config.id.clone(), 7);
        app.pending_restart_subscription = Some(config.id.clone());
        app.handle_event(AppEvent::SubscriptionUpdated {
            seq: 7,
            runtime: empty_runtime(config),
        })
        .await;
        assert!(app.pending_restart_subscription.is_none());
    }

    #[tokio::test]
    async fn restart_in_progress_blocks_keys() {
        let (mut app, _dir) = test_app().await;
        app.nodes = vec![node("one"), node("two")];
        app.page = Page::Nodes;
        app.selected = 0;
        app.restart_in_progress = true;

        app.handle_key(key(KeyCode::Char('j'))).await;
        assert_eq!(app.selected, 0);
        app.handle_key(key(KeyCode::Char('l'))).await;
        assert_eq!(app.page, Page::Nodes);
        app.handle_key(key(KeyCode::Char('q'))).await;
        assert!(!app.should_quit);
        app.handle_key(key(KeyCode::Char('s'))).await;
        assert_eq!(app.status, ProxyStatus::Stopped);
    }

    #[tokio::test]
    async fn connections_enter_copies_the_selected_target() {
        let (mut app, _dir) = test_app().await;
        let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
        app.handle_event(AppEvent::ClientAttached {
            id: 1,
            viewer: true,
            tx,
        })
        .await;
        app.page = Page::Connections;
        app.metrics
            .connections
            .push(crate::singbox::clash::ConnectionInfo {
                id: "id1".to_string(),
                host: "example.com".to_string(),
                destination_ip: "203.0.113.5".to_string(),
                destination_port: "443".to_string(),
                network: "tcp".to_string(),
                upload: 1,
                download: 2,
                duration_secs: None,
            });
        app.selected = 0;
        app.handle_key(key(KeyCode::Enter)).await;
        assert_eq!(app.message.as_deref(), Some(app.text().settings_copied));
        let mut copied = None;
        while let Ok(message) = rx.try_recv() {
            if let ServerMsg::Copy { text } = message {
                copied = Some(text);
            }
        }
        assert_eq!(copied.as_deref(), Some("example.com:443"));
    }

    #[tokio::test]
    async fn setup_copies_the_binary_path() {
        let (mut app, _dir) = test_app().await;
        let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
        app.handle_event(AppEvent::ClientAttached {
            id: 1,
            viewer: true,
            tx,
        })
        .await;
        app.page = Page::Setup;
        let expected = app.paths.binary().display().to_string();
        app.handle_key(key(KeyCode::Char('p'))).await;
        assert_eq!(app.message.as_deref(), Some(app.text().settings_copied));
        let mut copied = None;
        while let Ok(message) = rx.try_recv() {
            if let ServerMsg::Copy { text } = message {
                copied = Some(text);
            }
        }
        assert_eq!(copied.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn logs_follow_tail_until_scrolled() {
        let (mut app, _dir) = test_app().await;
        for index in 0..20 {
            app.logger.app(LogLevel::Info, format!("line {index}"));
        }
        app.page = Page::Logs;
        app.log_follow = true;
        app.selected = app.max_selection().saturating_sub(1);
        assert_eq!(app.selected, 19);

        app.handle_key(key(KeyCode::Char('k'))).await;
        assert!(!app.log_follow);
        assert_eq!(app.selected, 18);
        app.handle_key(key(KeyCode::Char('j'))).await;
        assert!(app.log_follow);
        assert_eq!(app.selected, 19);
        app.handle_key(key(KeyCode::Char('k'))).await;
        assert!(!app.log_follow);
        assert_eq!(app.selected, 18);
        app.handle_key(key(KeyCode::Char('G'))).await;
        assert!(app.log_follow);
        assert_eq!(app.selected, 19);
        app.handle_key(key(KeyCode::Char('f'))).await;
        assert!(!app.log_follow);
    }

    #[tokio::test]
    async fn logs_source_cycles_and_filters_lines() {
        let (mut app, _dir) = test_app().await;
        app.logger.app(LogLevel::Info, "app line");
        app.logger.singbox(LogLevel::Info, "sing-box line");
        app.page = Page::Logs;
        app.log_follow = true;
        app.selected = app.max_selection().saturating_sub(1);

        assert_eq!(app.log_source, LogSource::All);
        assert_eq!(app.log_lines().len(), 2);

        app.handle_key(key(KeyCode::Char('x'))).await;
        assert_eq!(app.log_source, LogSource::App);
        assert_eq!(
            app.message.as_deref(),
            Some(format!("{}: app", app.text().logs_source).as_str())
        );
        assert_eq!(app.log_lines().len(), 1);
        assert_eq!(app.log_lines()[0].message, "app line");
        assert_eq!(app.selected, 0);

        app.handle_key(key(KeyCode::Char('x'))).await;
        assert_eq!(app.log_source, LogSource::Singbox);
        assert_eq!(app.log_lines().len(), 1);
        assert_eq!(app.log_lines()[0].message, "sing-box line");

        app.handle_key(key(KeyCode::Char('x'))).await;
        assert_eq!(app.log_source, LogSource::All);
        assert_eq!(app.log_lines().len(), 2);
    }

    #[tokio::test]
    async fn nodes_sort_by_auto_candidates_and_latency_then_name() {
        let (mut app, _dir) = test_app().await;
        app.nodes = vec![node("Alpha"), node("Beta"), node("Gamma"), node("Delta")];
        for item in &mut app.nodes {
            item.tag = format!("tag-{}", item.name);
        }
        app.config.keywords_include = vec!["Alpha".to_string(), "Beta".to_string()];
        for item in &app.nodes {
            let delay = match item.name.as_str() {
                "Alpha" => 100,
                "Beta" => 20,
                "Delta" => 50,
                _ => 0,
            };
            if delay > 0 {
                app.node_delays.insert(item.tag.clone(), delay);
            }
        }
        app.page = Page::Nodes;
        app.selected = app
            .nodes
            .iter()
            .position(|item| item.name == "Delta")
            .unwrap();
        app.sort_nodes_preserving_selection();
        assert_eq!(
            app.nodes
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Beta", "Alpha", "Delta", "Gamma"]
        );
        assert_eq!(app.nodes[app.selected].name, "Delta");

        app.handle_key(key(KeyCode::Char('n'))).await;
        assert_eq!(
            app.nodes
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Beta", "Delta", "Gamma"]
        );
        assert_eq!(app.nodes[app.selected].name, "Delta");

        app.handle_key(key(KeyCode::Char('n'))).await;
        assert_eq!(
            app.nodes
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Beta", "Alpha", "Delta", "Gamma"]
        );
    }

    #[tokio::test]
    async fn restart_requires_two_presses_and_can_be_cancelled() {
        let (mut app, _dir) = test_app().await;
        app.status = ProxyStatus::Running;
        app.handle_key(key(KeyCode::Char('R'))).await;
        assert!(app.restart_confirm);
        assert!(app.view().restart_confirm);
        app.handle_key(key(KeyCode::Esc)).await;
        assert!(!app.restart_confirm);

        app.status = ProxyStatus::Stopped;
        app.handle_key(key(KeyCode::Char('R'))).await;
        assert!(!app.restart_confirm);
        assert_eq!(
            app.message.as_deref(),
            Some(app.text().msg_proxy_not_running)
        );
    }

    #[tokio::test]
    async fn logs_timestamp_cycles_through_time_date_and_hidden() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Logs;
        assert_eq!(app.log_timestamp, LogTimestamp::Time);
        app.handle_key(key(KeyCode::Char('d'))).await;
        assert_eq!(app.log_timestamp, LogTimestamp::DateTime);
        app.handle_key(key(KeyCode::Char('d'))).await;
        assert_eq!(app.log_timestamp, LogTimestamp::Hidden);
        app.handle_key(key(KeyCode::Char('d'))).await;
        assert_eq!(app.log_timestamp, LogTimestamp::Time);
    }

    #[tokio::test]
    async fn modified_shortcuts_are_ignored() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Nodes;
        app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL))
            .await;
        assert_eq!(app.page, Page::Nodes);
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT))
            .await;
        assert!(app.message.is_none());
    }

    #[test]
    fn listen_addresses_use_the_actual_ipv6_binding() {
        assert_eq!(
            format_listen_addresses("127.0.0.1", 12000, true),
            "127.0.0.1:12000 + [::1]:12000"
        );
        assert_eq!(
            format_listen_addresses("0.0.0.0", 12000, false),
            "0.0.0.0:12000"
        );
    }

    #[tokio::test]
    async fn stale_subscription_refresh_results_are_ignored() {
        let (mut app, _dir) = test_app().await;
        let config = SubscriptionConfig {
            id: "sub".to_string(),
            name: "sub".to_string(),
            url: "https://example.com/sub".to_string(),
            enabled: true,
        };
        app.config.subscriptions = vec![config.clone()];
        app.subscription_seq = 2;
        app.subscription_refresh_seq
            .insert(config.id.clone(), app.subscription_seq);

        let mut stale = empty_runtime(config.clone());
        stale.nodes = vec![node("stale")];
        app.handle_event(AppEvent::SubscriptionUpdated {
            seq: 1,
            runtime: stale,
        })
        .await;
        assert!(app.subscriptions.is_empty());

        let mut fresh = empty_runtime(config);
        fresh.nodes = vec![node("fresh")];
        app.handle_event(AppEvent::SubscriptionUpdated {
            seq: app.subscription_seq,
            runtime: fresh,
        })
        .await;
        assert_eq!(app.subscriptions.len(), 1);
        assert_eq!(app.nodes[0].name, "fresh");
    }

    #[tokio::test]
    async fn config_save_failure_keeps_the_in_memory_config() {
        let (mut app, _dir) = test_app().await;
        let _ = std::fs::remove_file(&app.paths.config_file);
        std::fs::create_dir(&app.paths.config_file).unwrap();
        let language = app.config.language;
        app.toggle_language();
        assert_eq!(app.config.language, language);
        assert!(app.last_error.is_some());
    }

    #[tokio::test]
    async fn subscription_errors_are_not_cleared_by_another_success() {
        let (mut app, _dir) = test_app().await;
        let config_a = SubscriptionConfig {
            id: "a".to_string(),
            name: "a".to_string(),
            url: "https://example.com/a".to_string(),
            enabled: true,
        };
        let config_b = SubscriptionConfig {
            id: "b".to_string(),
            name: "b".to_string(),
            url: "https://example.com/b".to_string(),
            enabled: true,
        };
        app.config.subscriptions = vec![config_a.clone(), config_b.clone()];
        let mut failed = empty_runtime(config_a);
        failed.error = Some("HTTP 500".to_string());
        app.subscriptions = vec![failed, empty_runtime(config_b.clone())];
        app.refresh_last_error_from_subscriptions();
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("a: HTTP 500")));

        let seq = app.next_subscription_seq();
        app.subscription_refresh_seq
            .insert(config_b.id.clone(), seq);
        app.handle_event(AppEvent::SubscriptionUpdated {
            seq,
            runtime: empty_runtime(config_b),
        })
        .await;
        assert!(app
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("a: HTTP 500")));
    }

    #[tokio::test]
    async fn lan_access_defaults_off_and_toggle_warns() {
        let (mut app, _dir) = test_app().await;
        assert!(!app.config.allow_lan);
        assert!(app.listen_addresses().starts_with("127.0.0.1"));

        app.toggle_allow_lan();
        assert!(app.config.allow_lan);
        assert!(app.listen_addresses().starts_with("0.0.0.0"));
        assert!(app
            .message
            .as_deref()
            .unwrap_or_default()
            .contains(app.text().settings_lan_warning));

        app.toggle_allow_lan();
        assert!(!app.config.allow_lan);
    }

    #[tokio::test]
    async fn settings_jk_selects_and_enter_toggles_or_edits() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Settings;
        app.selected = setting_index(&app, SettingsItem::Language);
        let language = app.config.language;
        app.handle_key(key(KeyCode::Enter)).await;
        assert_eq!(app.config.language, language.toggle());

        app.selected = setting_index(&app, SettingsItem::AllowLan);
        app.handle_key(key(KeyCode::Enter)).await;
        assert!(app.config.allow_lan);

        app.handle_key(key(KeyCode::Char('j'))).await;
        assert_eq!(app.selected, setting_index(&app, SettingsItem::LanAuth));
        app.handle_key(key(KeyCode::Char('k'))).await;
        assert_eq!(app.selected, setting_index(&app, SettingsItem::AllowLan));

        app.selected = setting_index(&app, SettingsItem::KeywordsExclude);
        app.handle_key(key(KeyCode::Enter)).await;
        assert_eq!(
            app.keyword_form().map(|form| form.field),
            Some(KeywordField::Exclude)
        );
    }

    #[tokio::test]
    async fn test_url_is_read_only_and_copyable() {
        let (mut app, _dir) = test_app().await;
        let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
        app.handle_event(AppEvent::ClientAttached {
            id: 1,
            viewer: true,
            tx,
        })
        .await;
        app.page = Page::Settings;
        app.selected = setting_index(&app, SettingsItem::TestUrl);
        let expected = app.config.test_url.clone();
        app.handle_key(key(KeyCode::Enter)).await;
        assert!(app.settings_form().is_none());
        assert_eq!(app.message.as_deref(), Some(app.text().settings_copied));
        let mut copied = None;
        while let Ok(message) = rx.try_recv() {
            if let ServerMsg::Copy { text } = message {
                copied = Some(text);
            }
        }
        assert_eq!(copied.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn settings_read_only_item_copies_its_value() {
        let (mut app, _dir) = test_app().await;
        let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
        app.handle_event(AppEvent::ClientAttached {
            id: 1,
            viewer: true,
            tx,
        })
        .await;
        app.page = Page::Settings;
        app.selected = setting_index(&app, SettingsItem::ConfigPath);
        let expected = app.paths.config_file.display().to_string();
        app.handle_key(key(KeyCode::Enter)).await;
        assert!(app.settings_form().is_none());
        assert_eq!(app.message.as_deref(), Some(app.text().settings_copied));
        let mut copied = None;
        while let Ok(message) = rx.try_recv() {
            if let ServerMsg::Copy { text } = message {
                copied = Some(text);
            }
        }
        assert_eq!(copied.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn port_and_dns_are_read_only_and_copyable() {
        let (mut app, _dir) = test_app().await;
        let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
        app.handle_event(AppEvent::ClientAttached {
            id: 1,
            viewer: true,
            tx,
        })
        .await;
        app.page = Page::Settings;
        for (item, expected) in [
            (SettingsItem::Port, "12000".to_string()),
            (SettingsItem::DnsServers, "1.1.1.1, 8.8.8.8".to_string()),
        ] {
            app.selected = setting_index(&app, item);
            app.handle_key(key(KeyCode::Enter)).await;
            assert!(app.settings_form().is_none());
            let mut copied = None;
            while let Ok(message) = rx.try_recv() {
                if let ServerMsg::Copy { text } = message {
                    copied = Some(text);
                }
            }
            assert_eq!(copied, Some(expected));
        }
    }

    #[tokio::test]
    async fn lan_auth_is_one_setting_and_persists_both_fields() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Settings;
        let base = settings_items(false).len();
        assert_eq!(settings_items(true).len(), base + 1);
        assert!(!settings_items(false).contains(&SettingsItem::LanAuth));

        app.toggle_allow_lan();
        assert!(app.config.allow_lan);
        assert!(settings_items(true).contains(&SettingsItem::LanAuth));

        app.selected = setting_index(&app, SettingsItem::LanAuth);
        app.handle_key(key(KeyCode::Enter)).await;
        let form = app.settings_form().expect("LAN auth form");
        assert_eq!(form.active, SettingsField::LanUsername);
        assert!(form.username.value.is_empty());
        assert!(form.password.value.is_empty());
        assert_eq!(form.title(app.text()), app.text().settings_lan_auth_title);

        if let Some(form) = app.settings_form_mut() {
            form.username = LineInput::new("alice");
        }
        app.handle_key(key(KeyCode::Tab)).await;
        assert_eq!(
            app.settings_form().unwrap().active,
            SettingsField::LanPassword
        );
        if let Some(form) = app.settings_form_mut() {
            form.password = LineInput::new("s3cret");
        }
        app.handle_key(key(KeyCode::Enter)).await;
        assert_eq!(app.config.lan_username, "alice");
        assert_eq!(app.config.lan_password, "s3cret");
        assert_eq!(app.config.lan_auth(), Some(("alice", "s3cret")));

        let (loaded, warnings) = Config::load_or_default(&app.paths.config_file);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(loaded.lan_auth(), Some(("alice", "s3cret")));

        let view = app.view();
        assert!(view.lan_password_set);
        assert!(view.config.lan_password.is_empty());
        assert!(view.lan_auth_configured());
    }

    #[tokio::test]
    async fn lan_auth_form_keeps_password_visible_and_ignores_ctrl_r() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Settings;
        app.config.allow_lan = true;
        app.config.lan_username = "alice".into();
        app.config.lan_password = "s3cret".into();
        app.selected = setting_index(&app, SettingsItem::LanAuth);
        app.handle_key(key(KeyCode::Enter)).await;
        let form = app.settings_form().expect("LAN auth form");
        assert_eq!(form.username.value, "alice");
        assert_eq!(form.password.value, "s3cret");
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
            .await;
        assert_eq!(app.settings_form().unwrap().password.value, "s3cret");
        assert_eq!(
            app.settings_form().unwrap().active,
            SettingsField::LanUsername
        );
    }

    #[tokio::test]
    async fn singbox_log_level_is_read_only_on_setup() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Setup;
        let before = app.config.singbox_log_level;
        app.handle_key(key(KeyCode::Char('v'))).await;
        assert_eq!(app.config.singbox_log_level, before);
        let (loaded, _) = Config::load_or_default(&app.paths.config_file);
        assert_eq!(loaded.singbox_log_level, before);
    }

    #[tokio::test]
    async fn q_detaches_viewers_without_stopping_daemon() {
        let (mut app, _dir) = test_app().await;
        let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
        app.handle_event(AppEvent::ClientAttached {
            id: 7,
            viewer: true,
            tx,
        })
        .await;
        app.handle_key(key(KeyCode::Char('q'))).await;
        assert!(drain_until_detach(&mut rx));
        assert!(!app.should_quit);
        assert!(app.clients.is_empty());

        let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
        app.handle_event(AppEvent::ClientAttached {
            id: 8,
            viewer: true,
            tx,
        })
        .await;
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .await;
        assert!(drain_until_detach(&mut rx));
        assert!(!app.should_quit);
        assert!(app.clients.is_empty());
    }

    fn drain_until_detach(rx: &mut mpsc::Receiver<ServerMsg>) -> bool {
        while let Ok(message) = rx.try_recv() {
            if matches!(message, ServerMsg::Detach) {
                return true;
            }
        }
        false
    }

    #[tokio::test]
    async fn status_request_reports_mode_and_proxy_state() {
        let (mut app, _dir) = test_app().await;
        app.status = ProxyStatus::Running;
        app.managed = true;
        app.service = Some(crate::service::ServiceIdentity {
            manager: crate::service::ServiceManager::SystemdUser,
            unit: "dfrelay.service".into(),
            target: None,
            uid: crate::service::current_uid(),
        });
        let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
        app.handle_event(AppEvent::ClientAttached {
            id: 9,
            viewer: false,
            tx,
        })
        .await;
        app.handle_event(AppEvent::StatusRequest { client_id: 9 })
            .await;
        let Ok(ServerMsg::Status(status)) = rx.try_recv() else {
            panic!("expected status response");
        };
        assert!(status.managed);
        assert_eq!(status.proxy, ProxyStatus::Running);
        assert_eq!(status.pid, std::process::id());
        assert_eq!(
            status.service.as_ref().map(|service| service.unit.as_str()),
            Some("dfrelay.service")
        );
    }

    #[tokio::test]
    async fn restart_backoff_pauses_after_five_failures() {
        let (mut app, _dir) = test_app().await;
        for _ in 0..MAX_RESTART_ATTEMPTS {
            app.schedule_restart();
            assert!(app.restart_at.is_some());
        }
        assert_eq!(app.restart_attempts, MAX_RESTART_ATTEMPTS);
        app.schedule_restart();
        assert!(app.restart_at.is_none());
        assert_eq!(app.message.as_deref(), Some(app.text().msg_restart_paused));
    }

    #[tokio::test]
    async fn subscription_form_validates_and_saves_disabled_entry() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Subscriptions;
        app.overlay = Some(Overlay::Edit(EditForm::Subscription(
            SubscriptionForm::new(),
        )));
        if let Some(form) = app.subscription_form_mut() {
            form.url = LineInput::new("ftp://example.com/sub");
        }
        app.save_subscription_form();
        assert!(app.config.subscriptions.is_empty());
        assert!(app
            .subscription_form()
            .and_then(|form| form.error.as_ref())
            .is_some());

        if let Some(form) = app.subscription_form_mut() {
            form.url = LineInput::new("https://example.com/sub");
            form.enabled = false;
        }
        app.save_subscription_form();
        assert!(app.subscription_form().is_none());
        assert_eq!(app.config.subscriptions.len(), 1);
        assert!(!app.config.subscriptions[0].enabled);
        assert_eq!(app.subscriptions.len(), 1);
        assert!(app.config.subscriptions[0].name.contains("example.com"));

        let id = app.config.subscriptions[0].id.clone();
        app.delete_subscription(&id);
        assert!(app.config.subscriptions.is_empty());
        assert!(app.subscriptions.is_empty());
    }

    #[tokio::test]
    async fn subscription_form_clears_its_error_when_the_user_edits() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Subscriptions;
        app.overlay = Some(Overlay::Edit(EditForm::Subscription(
            SubscriptionForm::new(),
        )));
        if let Some(form) = app.subscription_form_mut() {
            form.url = LineInput::new("ftp://example.com/sub");
        }
        app.save_subscription_form();
        assert!(app.subscription_form().unwrap().error.is_some());

        app.handle_key(key(KeyCode::Char('x'))).await;
        assert!(app.subscription_form().unwrap().error.is_none());
    }

    #[tokio::test]
    async fn subscription_delete_confirmation_is_persistent_and_cancellable() {
        let (mut app, _dir) = test_app().await;
        let config = SubscriptionConfig {
            id: "s-1".to_string(),
            name: "test".to_string(),
            url: "https://example.com/sub".to_string(),
            enabled: false,
        };
        app.config.subscriptions.push(config.clone());
        app.subscriptions.push(empty_runtime(config));
        app.page = Page::Subscriptions;
        app.selected = 0;

        app.handle_key(key(KeyCode::Char('d'))).await;
        assert!(app.subscription_delete.is_some());
        assert!(app.view().subscription_delete);
        assert!(
            app.message.is_none(),
            "confirmations must not use the toast"
        );

        app.message = Some("expired toast".to_string());
        app.message_at = Some(Instant::now() - TOAST_TTL - Duration::from_secs(1));
        app.on_tick().await;
        assert!(
            app.subscription_delete.is_some(),
            "toast expiry must not cancel the confirmation"
        );

        app.handle_key(key(KeyCode::Char('j'))).await;
        assert!(app.subscription_delete.is_none());

        app.handle_key(key(KeyCode::Char('d'))).await;
        app.handle_key(key(KeyCode::Esc)).await;
        assert!(app.subscription_delete.is_none());
        assert_eq!(app.config.subscriptions.len(), 1);

        app.handle_key(key(KeyCode::Char('d'))).await;
        assert!(app.subscription_delete.is_some());
        app.handle_key(key(KeyCode::Char('d'))).await;
        assert!(app.config.subscriptions.is_empty());
    }

    #[tokio::test]
    async fn node_selection_while_stopped_applies_on_start() {
        let (mut app, _dir) = test_app().await;
        app.nodes = vec![node("香港 01"), node("日本 02")];
        app.page = Page::Nodes;
        app.selected = 1;

        app.handle_key(key(KeyCode::Enter)).await;
        assert_eq!(
            app.preferred_node.as_deref(),
            Some(app.nodes[1].tag.as_str())
        );
        assert_eq!(
            app.pending_selection,
            Some(PendingSelection::Node(app.nodes[1].tag.clone()))
        );
        assert!(app
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("日本 02"));

        app.handle_key(key(KeyCode::Char('a'))).await;
        assert!(app.preferred_node.is_none());
        assert_eq!(app.pending_selection, Some(PendingSelection::Auto));
    }

    #[tokio::test]
    async fn current_node_falls_back_to_name_when_stopped() {
        let (mut app, _dir) = test_app().await;
        app.nodes = vec![node("香港 01")];
        app.preferred_node = Some(app.nodes[0].tag.clone());
        assert_eq!(app.current_node().as_deref(), Some("香港 01"));

        app.preferred_node = None;
        assert_eq!(app.current_node().as_deref(), Some(app.text().common_auto));
    }

    #[tokio::test]
    async fn current_node_labels_removed_and_unknown_tags() {
        let (mut app, _dir) = test_app().await;
        app.nodes.clear();
        app.proxy_node_names
            .insert("n-old".to_string(), "香港 01".to_string());
        app.metrics.selection.selected = Some("n-old".to_string());
        assert_eq!(
            app.current_node(),
            Some(format!("香港 01{}", app.text().node_removed_suffix))
        );

        app.metrics.selection.selected = Some("n-missing".to_string());
        assert_eq!(app.current_node().as_deref(), Some(app.text().node_unknown));
    }

    #[tokio::test]
    async fn selection_mode_follows_selector_state_and_pending_node() {
        let (mut app, _dir) = test_app().await;
        // Stopped without a pending node: automatic selection.
        assert!(app.selection_mode_auto());

        app.preferred_node = Some("n1".to_string());
        assert!(!app.selection_mode_auto());

        // While running the selector state is authoritative.
        app.metrics.selection.selected = Some("auto".to_string());
        assert!(app.selection_mode_auto());

        app.metrics.selection.selected = Some("n2".to_string());
        assert!(!app.selection_mode_auto());
    }

    #[tokio::test]
    async fn keyword_form_saves_each_field_separately() {
        let (mut app, _dir) = test_app().await;
        app.nodes = vec![node("香港 01"), node("日本 02")];
        app.config.keywords_exclude = vec!["过期".to_string()];
        let mut form = KeywordForm::from_config(&app.config, KeywordField::Include);
        form.value = LineInput::new("香港, HK");
        app.overlay = Some(Overlay::Edit(EditForm::Keyword(form)));

        app.save_keyword_form();

        assert_eq!(app.config.keywords_include, vec!["香港", "HK"]);
        assert_eq!(app.config.keywords_exclude, vec!["过期"]);
        assert!(app.keyword_form().is_none());
        let message = app.message.as_deref().unwrap_or_default();
        assert!(message.contains("1/2"), "{message}");

        let mut form = KeywordForm::from_config(&app.config, KeywordField::Exclude);
        form.value = LineInput::new("过期, 剩余");
        app.overlay = Some(Overlay::Edit(EditForm::Keyword(form)));
        app.save_keyword_form();
        assert_eq!(app.config.keywords_include, vec!["香港", "HK"]);
        assert_eq!(app.config.keywords_exclude, vec!["过期", "剩余"]);
    }

    #[tokio::test]
    async fn missing_preferred_node_is_cleared() {
        let (mut app, _dir) = test_app().await;
        app.nodes = vec![node("香港 01")];
        app.preferred_node = Some("n-missing".to_string());
        app.pending_selection = Some(PendingSelection::Node("n-missing".to_string()));
        app.sync_preferred_node();
        assert!(app.preferred_node.is_none());
        assert!(app.pending_selection.is_none());
        assert_eq!(
            app.message.as_deref(),
            Some(app.text().msg_preferred_missing)
        );
    }

    #[tokio::test]
    async fn help_returns_to_the_previous_page() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Logs;

        app.handle_key(key(KeyCode::Char('?'))).await;
        assert_eq!(app.page, Page::Help);
        assert_eq!(app.view().tab_page, Page::Logs);
        app.handle_key(key(KeyCode::Esc)).await;
        assert_eq!(app.page, Page::Logs);

        app.handle_key(key(KeyCode::Char('?'))).await;
        assert_eq!(app.page, Page::Help);
        app.handle_key(key(KeyCode::Char('?'))).await;
        assert_eq!(app.page, Page::Logs);
    }

    #[tokio::test]
    async fn help_scrolls_with_jk_and_clamps_at_bottom() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Logs;
        app.handle_key(key(KeyCode::Char('?'))).await;
        app.help_max_scroll = 5;

        app.handle_key(key(KeyCode::Char('j'))).await;
        assert_eq!(app.help_scroll, 1);
        app.handle_key(key(KeyCode::Down)).await;
        assert_eq!(app.help_scroll, 2);
        app.handle_key(key(KeyCode::Char('k'))).await;
        assert_eq!(app.help_scroll, 1);
        app.handle_key(key(KeyCode::Char('g'))).await;
        assert_eq!(app.help_scroll, 0);
        app.handle_key(key(KeyCode::Char('G'))).await;
        assert_eq!(app.help_scroll, 5);
        for _ in 0..10 {
            app.handle_key(key(KeyCode::Char('j'))).await;
        }
        assert_eq!(app.help_scroll, 5);
        app.handle_key(key(KeyCode::Char('k'))).await;
        assert_eq!(app.help_scroll, 4);

        app.handle_event(AppEvent::HelpViewport(2)).await;
        assert_eq!(app.help_scroll, 2);
    }

    #[test]
    fn line_input_moves_cursor_and_edits_at_position() {
        let mut input = LineInput::new("abcd");
        assert_eq!(input.cursor, 4);
        input.left();
        input.left();
        assert_eq!(input.cursor, 2);
        input.insert('X');
        assert_eq!(input.value, "abXcd");
        assert_eq!(input.cursor, 3);
        input.backspace();
        assert_eq!(input.value, "abcd");
        assert_eq!(input.cursor, 2);
        input.delete();
        assert_eq!(input.value, "abd");
        input.home();
        input.delete();
        assert_eq!(input.value, "bd");
        assert_eq!(input.cursor, 0);
        input.end();
        input.insert('尾');
        assert_eq!(input.value, "bd尾");
        assert_eq!(input.cursor, 3);
        input.left();
        input.insert('中');
        assert_eq!(input.value, "bd中尾");
    }

    #[tokio::test]
    async fn enter_opens_log_detail_and_esc_closes_it() {
        let (mut app, _dir) = test_app().await;
        app.logger.app(LogLevel::Info, "a detailed log line");
        app.page = Page::Logs;
        app.log_follow = true;
        app.selected = app.max_selection().saturating_sub(1);

        app.handle_key(key(KeyCode::Enter)).await;
        assert!(app.log_detail().is_some());
        app.handle_key(key(KeyCode::Esc)).await;
        assert!(app.log_detail().is_none());
    }

    #[tokio::test]
    async fn log_detail_scrolls_and_clamps_to_the_viewport() {
        let (mut app, _dir) = test_app().await;
        for index in 0..20 {
            app.logger
                .app(LogLevel::Info, format!("scrolling line {index}"));
        }
        app.page = Page::Logs;
        app.log_follow = true;
        app.selected = app.max_selection().saturating_sub(1);
        app.handle_key(key(KeyCode::Enter)).await;
        assert!(app.log_detail().is_some());

        app.handle_event(AppEvent::LogDetailViewport(5)).await;
        app.handle_key(key(KeyCode::Char('j'))).await;
        assert_eq!(app.log_detail_scroll(), 1);
        app.handle_key(key(KeyCode::Down)).await;
        assert_eq!(app.log_detail_scroll(), 2);
        app.handle_key(key(KeyCode::Char('k'))).await;
        assert_eq!(app.log_detail_scroll(), 1);
        app.handle_key(key(KeyCode::Char('g'))).await;
        assert_eq!(app.log_detail_scroll(), 0);
        app.handle_key(key(KeyCode::Char('G'))).await;
        assert_eq!(app.log_detail_scroll(), 5);
        app.handle_key(key(KeyCode::Char('j'))).await;
        assert_eq!(app.log_detail_scroll(), 5);
        app.handle_key(key(KeyCode::End)).await;
        assert_eq!(app.log_detail_scroll(), 5);
        app.handle_key(key(KeyCode::Char('k'))).await;
        assert_eq!(app.log_detail_scroll(), 4);
        app.handle_event(AppEvent::LogDetailViewport(2)).await;
        assert_eq!(app.log_detail_scroll(), 2);
        app.handle_key(key(KeyCode::Esc)).await;
        assert!(app.log_detail().is_none());
    }

    #[tokio::test]
    async fn toast_expires_after_ttl() {
        let (mut app, _dir) = test_app().await;
        assert_eq!(TOAST_TTL, Duration::from_secs(3));
        app.notify("saved");
        assert!(app.view().toast.is_some());
        app.message_at = Some(Instant::now() - TOAST_TTL - Duration::from_secs(1));
        assert!(app.view().toast.is_none());
    }

    #[tokio::test]
    async fn dashboard_scroll_clamps_to_viewport() {
        let (mut app, _dir) = test_app().await;
        app.page = Page::Dashboard;
        app.handle_event(AppEvent::DashboardViewport(3)).await;
        for _ in 0..10 {
            app.handle_key(key(KeyCode::Char('j'))).await;
        }
        assert_eq!(app.dashboard_scroll, 3);
        app.handle_key(key(KeyCode::Char('k'))).await;
        assert_eq!(app.dashboard_scroll, 2);
        app.handle_key(key(KeyCode::Char('g'))).await;
        assert_eq!(app.dashboard_scroll, 0);
        app.handle_key(key(KeyCode::Char('G'))).await;
        assert_eq!(app.dashboard_scroll, 3);
        app.handle_event(AppEvent::DashboardViewport(1)).await;
        assert_eq!(app.dashboard_scroll, 1);
    }

    #[tokio::test]
    async fn view_roundtrips_through_json() {
        let (mut app, _dir) = test_app().await;
        app.node_delays.insert("n1".to_string(), 42);
        app.delay_testing.insert("n2".to_string());
        let encoded = serde_json::to_string(&app.view()).unwrap();
        let decoded: View = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.page, app.page);
        assert_eq!(decoded.config.port, app.config.port);
        assert_eq!(decoded.node_delays.get("n1"), Some(&42));
        assert!(decoded.delay_testing.contains("n2"));
    }

    #[tokio::test]
    async fn metrics_node_delays_feed_the_nodes_view() {
        let (mut app, _dir) = test_app().await;
        let mut metrics = MetricsSnapshot::default();
        metrics.node_delays.insert("n1".to_string(), 42);
        app.handle_event(AppEvent::Metrics(metrics)).await;
        assert_eq!(app.node_delays.get("n1"), Some(&42));
        assert_eq!(app.view().node_delays.get("n1"), Some(&42));
    }

    fn tagged_node(name: &str, tag: &str) -> Node {
        let mut item = node(name);
        item.tag = tag.to_string();
        item
    }

    fn health_outcome_for(status: u16) -> HealthOutcome {
        health_outcome(&Err(ClashError::Http(status)))
    }

    #[test]
    fn health_probe_outcomes_distinguish_node_and_api_errors() {
        assert_eq!(health_outcome(&Ok(120)), HealthOutcome::Healthy(120));
        assert_eq!(health_outcome(&Ok(0)), HealthOutcome::Failed);
        for status in [408, 500, 502, 503, 504] {
            assert_eq!(health_outcome_for(status), HealthOutcome::Failed);
        }
        for status in [400, 401, 404, 429] {
            assert_eq!(health_outcome_for(status), HealthOutcome::Unavailable);
        }
    }

    #[test]
    fn failover_keeps_auto_mode_and_picks_fastest_manual_node() {
        let healthy = vec![
            ("n-fast".to_string(), 40),
            ("n-slow".to_string(), 120),
            ("n-dead".to_string(), 300),
        ];
        assert_eq!(
            choose_recovery_target(Some("auto"), &healthy),
            Some(RecoveryTarget::Auto)
        );
        assert_eq!(
            choose_recovery_target(Some("n-manual"), &healthy),
            Some(RecoveryTarget::Node("n-fast".to_string()))
        );
        assert_eq!(choose_recovery_target(Some("auto"), &[]), None);
    }

    #[tokio::test]
    async fn health_probe_failures_are_counted_per_selected_node() {
        let (mut app, _dir) = test_app().await;
        app.status = ProxyStatus::Running;
        app.nodes = vec![tagged_node("A", "n1"), tagged_node("B", "n2")];
        app.proxy_node_tags = vec!["n1".to_string(), "n2".to_string()];
        app.metrics.selection.selected = Some("n1".to_string());

        let generation = app.health_generation;
        for _ in 0..HEALTH_FAILURE_THRESHOLD {
            app.handle_event(AppEvent::HealthProbe {
                generation,
                tag: "n1".to_string(),
                outcome: HealthOutcome::Failed,
            })
            .await;
        }
        assert_eq!(app.health_failures, HEALTH_FAILURE_THRESHOLD);
        assert_eq!(app.health_failure_tag.as_deref(), Some("n1"));

        app.handle_event(AppEvent::HealthProbe {
            generation,
            tag: "n1".to_string(),
            outcome: HealthOutcome::Healthy(35),
        })
        .await;
        assert_eq!(app.health_failures, 0);
        assert!(app.health_failure_tag.is_none());
        assert_eq!(app.node_delays.get("n1"), Some(&35));

        app.handle_event(AppEvent::HealthProbe {
            generation,
            tag: "n1".to_string(),
            outcome: HealthOutcome::Failed,
        })
        .await;
        assert_eq!(app.health_failures, 1);
        app.metrics.selection.selected = Some("n2".to_string());
        app.handle_event(AppEvent::HealthProbe {
            generation,
            tag: "n1".to_string(),
            outcome: HealthOutcome::Failed,
        })
        .await;
        assert_eq!(app.health_failures, 0);
    }

    #[tokio::test]
    async fn only_new_proxy_connections_keep_health_probing_active() {
        let (mut app, _dir) = test_app().await;
        let mut metrics = MetricsSnapshot::default();
        metrics
            .connections
            .push(crate::singbox::clash::ConnectionInfo {
                id: "c1".to_string(),
                ..Default::default()
            });
        app.handle_event(AppEvent::Metrics(metrics.clone())).await;
        let first = app.last_health_activity.expect("first connection");

        app.last_health_activity = Some(Instant::now() - Duration::from_secs(10));
        let stale = app.last_health_activity.unwrap();
        app.handle_event(AppEvent::Metrics(metrics)).await;
        assert_eq!(app.last_health_activity, Some(stale));

        let mut next = MetricsSnapshot::default();
        next.connections
            .push(crate::singbox::clash::ConnectionInfo {
                id: "c2".to_string(),
                ..Default::default()
            });
        app.handle_event(AppEvent::Metrics(next)).await;
        assert!(app.last_health_activity.expect("new connection") > stale);
        assert!(app.last_health_activity.expect("new connection") >= first);
    }

    #[tokio::test]
    async fn stale_health_probe_events_are_ignored() {
        let (mut app, _dir) = test_app().await;
        app.status = ProxyStatus::Running;
        app.nodes = vec![tagged_node("A", "n1")];
        app.proxy_node_tags = vec!["n1".to_string()];
        app.metrics.selection.selected = Some("n1".to_string());
        let generation = app.health_generation;
        app.invalidate_health_tasks();

        app.handle_event(AppEvent::HealthProbe {
            generation,
            tag: "n1".to_string(),
            outcome: HealthOutcome::Failed,
        })
        .await;
        assert_eq!(app.health_failures, 0);
    }

    #[tokio::test]
    async fn recovery_candidates_respect_auto_filter_and_latency_order() {
        let (mut app, _dir) = test_app().await;
        app.status = ProxyStatus::Running;
        app.nodes = vec![
            tagged_node("香港 A", "n1"),
            tagged_node("日本 B", "n2"),
            tagged_node("香港 C", "n3"),
        ];
        app.proxy_node_tags = vec!["n1".to_string(), "n2".to_string(), "n3".to_string()];
        app.config.keywords_include = vec!["香港".to_string()];
        app.node_delays.insert("n2".to_string(), 10);
        app.node_delays.insert("n3".to_string(), 20);

        app.metrics.selection.selected = Some("auto".to_string());
        assert_eq!(app.recovery_candidates("n1"), vec!["n3".to_string()]);
        assert_eq!(app.recovery_candidates("n3"), vec!["n1".to_string()]);

        app.metrics.selection.selected = Some("n1".to_string());
        assert_eq!(
            app.recovery_candidates("n1"),
            vec!["n2".to_string(), "n3".to_string()]
        );
    }

    #[tokio::test]
    async fn no_healthy_nodes_reports_the_failure_without_enabled_subscriptions() {
        let (mut app, _dir) = test_app().await;
        app.status = ProxyStatus::Running;
        app.on_no_healthy_nodes();
        assert_eq!(
            app.last_error.as_deref(),
            Some(app.text().msg_health_no_nodes)
        );
        assert!(app.message.is_some());
        assert!(!app.recovery_refresh_pending);
        assert!(app.health_pause_until.is_some_and(|at| at > Instant::now()));
        app.invalidate_health_tasks();
        assert!(app.health_pause_until.is_none());
    }
}
