use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::i18n::{Lang, Text};
use crate::logging::{LogLevel, LogLine, LogSource, LogTimestamp};
use crate::service::ServiceIdentity;
use crate::singbox::clash::MetricsSnapshot;
use crate::singbox::tags;
use crate::state::{Overlay, Page, ProxyStatus, SetupState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub tag: String,
    pub name: String,
    pub source: String,
    pub protocol: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryStatus {
    NotInstalled,
    Installed,
    UpgradeAvailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionView {
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub node_count: usize,
    pub unsupported_count: usize,
    pub last_updated: Option<String>,
    pub error: Option<String>,
    pub stale: bool,
}

/// The subset of `Config` the TUI needs. Keeping this as a dedicated type
/// prevents future secret fields from being serialized into `View` by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigView {
    pub language: Lang,
    pub port: u16,
    pub allow_lan: bool,
    pub lan_username: String,
    pub dns_servers: Vec<String>,
    pub test_url: String,
    pub keywords_include: Vec<String>,
    pub keywords_exclude: Vec<String>,
    pub app_log_level: LogLevel,
    pub singbox_log_level: LogLevel,
}

impl ConfigView {
    pub fn from_config(config: &Config) -> Self {
        Self {
            language: config.language,
            port: config.port,
            allow_lan: config.allow_lan,
            lan_username: crate::text::sanitize_owned(config.lan_username.clone()),
            dns_servers: config.dns_servers.clone(),
            test_url: crate::text::sanitize_owned(config.test_url.clone()),
            keywords_include: config
                .keywords_include
                .iter()
                .cloned()
                .map(crate::text::sanitize_owned)
                .collect(),
            keywords_exclude: config
                .keywords_exclude
                .iter()
                .cloned()
                .map(crate::text::sanitize_owned)
                .collect(),
            app_log_level: config.app_log_level,
            singbox_log_level: config.singbox_log_level,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub managed: bool,
    #[serde(default)]
    pub service: Option<ServiceIdentity>,
    pub proxy: ProxyStatus,
    pub uptime_secs: u64,
    pub listen: String,
    pub current_node: Option<String>,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub config: ConfigView,
    pub lan_password_set: bool,
    pub page: Page,
    pub tab_page: Page,
    pub dashboard_scroll: usize,
    pub status: ProxyStatus,
    pub metrics_stale: bool,
    pub nodes: Vec<NodeView>,
    #[serde(default)]
    pub node_count: usize,
    pub auto_tags: HashSet<String>,
    pub auto_candidates: usize,
    pub subscriptions: Vec<SubscriptionView>,
    pub selected: usize,
    pub preferred_node: Option<String>,
    pub metrics: MetricsSnapshot,
    #[serde(default)]
    pub connection_count: usize,
    pub node_delays: HashMap<String, u64>,
    pub delay_testing: HashSet<String>,
    pub up_speed: f64,
    pub down_speed: f64,
    pub local_ips: Vec<String>,
    pub proxy_ips: Vec<String>,
    pub binary_version: Option<String>,
    pub binary_verified: Option<bool>,
    pub binary_status: BinaryStatus,
    pub latest_binary_version: Option<String>,
    pub previous_binary_available: bool,
    pub previous_binary_version: Option<String>,
    pub setup: SetupState,
    pub overlay: Option<Overlay>,
    pub subscription_delete: bool,
    pub log_source: LogSource,
    pub log_filter: LogLevel,
    pub log_follow: bool,
    pub log_timestamp: LogTimestamp,
    pub logs: Vec<LogLine>,
    pub log_file: PathBuf,
    pub last_error: Option<String>,
    pub toast: Option<String>,
    pub restart_confirm: bool,
    pub rollback_confirm: bool,
    pub restart_in_progress: bool,
    pub quitting: bool,
    pub help_scroll: usize,
    pub binary_path: String,
    pub config_path: String,
    pub inbox_path: String,
    pub uptime: String,
    pub listen: String,
    pub current_node: Option<String>,
    pub selection_mode_auto: bool,
}

impl View {
    pub fn text(&self) -> &'static Text {
        self.config.language.text()
    }

    pub fn language_label(&self) -> &'static str {
        match self.config.language {
            Lang::Zh => "中文",
            Lang::En => "English",
        }
    }

    pub fn uptime(&self) -> &str {
        &self.uptime
    }

    pub fn listen_addresses(&self) -> &str {
        &self.listen
    }

    pub fn current_node(&self) -> Option<&str> {
        self.current_node.as_deref()
    }

    pub fn lan_auth_configured(&self) -> bool {
        self.config.allow_lan && !self.config.lan_username.is_empty() && self.lan_password_set
    }

    pub fn current_delay(&self) -> Option<u64> {
        let selected = self.metrics.selection.selected.as_deref();
        let tag = match selected {
            Some(tags::AUTO) | None => self.metrics.selection.auto_now.as_deref(),
            Some(tag) => Some(tag),
        };
        if let Some(delay) = tag.and_then(|tag| self.node_delays.get(tag)) {
            return Some(*delay);
        }
        if selected.is_none() || selected == Some(tags::AUTO) {
            self.metrics.selection.auto_delay
        } else {
            None
        }
    }

    pub fn log_detail(&self) -> Option<&LogLine> {
        match &self.overlay {
            Some(Overlay::LogDetail { line, .. }) => Some(line),
            _ => None,
        }
    }
}
