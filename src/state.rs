//! UI state shared by the daemon reducer and the TUI client.
//!
//! These types are part of the control-protocol snapshot (`View`) and are also
//! mutated by the daemon's key handlers, so they live outside both `app` and
//! `view` to keep the dependency direction one-way:
//! `ui/client -> state`, `view -> state`, `app -> state`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{Config, SubscriptionConfig};
use crate::i18n::Text;
use crate::logging::LogLine;
use crate::singbox::binary::InboxCandidate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(usize)]
pub enum Page {
    Dashboard,
    Nodes,
    Subscriptions,
    Connections,
    Logs,
    Settings,
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
            Self::Dashboard => text.dashboard_title,
            Self::Nodes => text.nodes_title,
            Self::Subscriptions => text.subscriptions_title,
            Self::Connections => text.connections_title,
            Self::Logs => text.logs_title,
            Self::Settings => text.settings_title,
            Self::Help => text.help_title,
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
    pub(crate) const FOCUS_COUNT: usize = 3;

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
    AppUpdate,
    BinaryInfo,
    ConfigPath,
    LogFile,
}

pub const SETTINGS_ITEMS: [SettingsItem; 13] = [
    SettingsItem::Language,
    SettingsItem::Port,
    SettingsItem::AllowLan,
    SettingsItem::LanAuth,
    SettingsItem::DnsServers,
    SettingsItem::TestUrl,
    SettingsItem::KeywordsInclude,
    SettingsItem::KeywordsExclude,
    SettingsItem::AppLogLevel,
    SettingsItem::AppUpdate,
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
        text.edit_form_hint
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
            Self::Subscription(_) => text.edit_form_hint,
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
    Setup { selected: usize },
    LogDetail { line: LogLine, scroll: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayKind {
    Subscription,
    Keyword,
    Settings,
    Setup,
    LogDetail,
}

impl Overlay {
    pub(crate) fn kind(&self) -> OverlayKind {
        match self {
            Self::Edit(EditForm::Subscription(_)) => OverlayKind::Subscription,
            Self::Edit(EditForm::Keyword(_)) => OverlayKind::Keyword,
            Self::Edit(EditForm::Settings(_)) => OverlayKind::Settings,
            Self::Setup { .. } => OverlayKind::Setup,
            Self::LogDetail { .. } => OverlayKind::LogDetail,
        }
    }

    pub fn log_detail(&self) -> Option<(&LogLine, usize)> {
        match self {
            Self::LogDetail { line, scroll } => Some((line, *scroll)),
            Self::Edit(_) | Self::Setup { .. } => None,
        }
    }
}
