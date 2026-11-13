//! Page, selection, overlay, log-view and toast state.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NodeSort {
    #[default]
    AutoLatency,
    Name,
}

pub(crate) struct UiState {
    pub(crate) page: Page,
    pub(crate) dashboard_scroll: usize,
    pub(crate) dashboard_max_scroll: usize,
    pub(crate) selected: usize,
    pub(crate) page_selection: [usize; 7],
    pub(crate) log_detail_max_scroll: usize,
    pub(crate) subscription_delete: Option<String>,
    pub(crate) log_source: LogSource,
    pub(crate) log_filter: LogLevel,
    pub(crate) log_follow: bool,
    pub(crate) log_timestamp: LogTimestamp,
    pub(crate) last_error: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) message_at: Option<Instant>,
    pub(crate) restart_confirm: bool,
    pub(crate) rollback_confirm: bool,
    pub(crate) help_scroll: usize,
    pub(crate) help_max_scroll: usize,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) help_return: Option<Page>,
    pub(crate) node_sort: NodeSort,
    pub(crate) last_node_sort: Option<Instant>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            page: Page::Dashboard,
            dashboard_scroll: 0,
            dashboard_max_scroll: 0,
            selected: 0,
            page_selection: [0; 7],
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
            help_scroll: 0,
            help_max_scroll: 0,
            overlay: None,
            help_return: None,
            node_sort: NodeSort::default(),
            last_node_sort: None,
        }
    }
}
