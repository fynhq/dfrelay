use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::{Overlay, Page, ProxyStatus, SetupState};
use crate::config::Config;
use crate::i18n::{Lang, Text};
use crate::logging::{LogLevel, LogLine};
use crate::service::ServiceIdentity;
use crate::singbox::clash::MetricsSnapshot;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub config: Config,
    pub lan_password_set: bool,
    pub page: Page,
    pub tab_page: Page,
    pub dashboard_scroll: usize,
    pub status: ProxyStatus,
    pub metrics_stale: bool,
    pub nodes: Vec<NodeView>,
    pub auto_tags: HashSet<String>,
    pub auto_candidates: usize,
    pub subscriptions: Vec<SubscriptionView>,
    pub selected: usize,
    pub preferred_node: Option<String>,
    pub metrics: MetricsSnapshot,
    pub node_delays: HashMap<String, u64>,
    pub delay_testing: HashSet<String>,
    pub up_speed: f64,
    pub down_speed: f64,
    pub local_ips: Vec<String>,
    pub proxy_ips: Vec<String>,
    pub binary_version: Option<String>,
    pub binary_status: BinaryStatus,
    pub latest_binary_version: Option<String>,
    pub previous_binary_available: bool,
    pub previous_binary_version: Option<String>,
    pub setup: SetupState,
    pub overlay: Option<Overlay>,
    pub subscription_delete: bool,
    pub log_filter: LogLevel,
    pub log_follow: bool,
    pub log_show_timestamp: bool,
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
            Some("auto") | None => self.metrics.selection.auto_now.as_deref(),
            Some(tag) => Some(tag),
        };
        if let Some(delay) = tag.and_then(|tag| self.node_delays.get(tag)) {
            return Some(*delay);
        }
        if selected.is_none() || selected == Some("auto") {
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
