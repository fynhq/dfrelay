//! Key handling and edit-form interactions for the TUI.

use super::*;
use crate::logging::LogLine;
use crate::state::{KeywordField, OverlayKind, SettingsItem, MAIN_PAGES};
use std::time::{Duration, Instant};

impl App {
    pub(super) async fn handle_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.ui_state.overlay.is_some() {
                self.close_overlay();
                return;
            }
            self.ui_state.restart_confirm = false;
            self.ui_state.rollback_confirm = false;
            self.ui_state.subscription_delete = None;
            self.setup.pending_install = None;
            self.setup.message = None;
            self.detach_clients();
            return;
        }
        if self.proxy_runtime.restart_in_progress {
            if key.code == KeyCode::Char('q') {
                self.detach_clients();
            }
            return;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        if self.ui_state.rollback_confirm {
            match key.code {
                KeyCode::Char('b') => {
                    self.ui_state.rollback_confirm = false;
                    self.perform_rollback();
                    return;
                }
                KeyCode::Esc => {
                    self.ui_state.rollback_confirm = false;
                    return;
                }
                _ => {
                    self.ui_state.rollback_confirm = false;
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
        if let Some(kind) = self.ui_state.overlay.as_ref().map(Overlay::kind) {
            self.handle_overlay_key(kind, key).await;
            return;
        }
        if self.ui_state.restart_confirm {
            match key.code {
                KeyCode::Char('R') => {
                    self.ui_state.restart_confirm = false;
                    self.perform_restart();
                    return;
                }
                KeyCode::Esc => {
                    self.ui_state.restart_confirm = false;
                    return;
                }
                _ => {
                    self.ui_state.restart_confirm = false;
                    return;
                }
            }
        }
        if let Some(id) = self.ui_state.subscription_delete.clone() {
            match key.code {
                KeyCode::Char('d') => {
                    self.ui_state.subscription_delete = None;
                    self.delete_subscription(&id);
                    return;
                }
                KeyCode::Esc => {
                    self.ui_state.subscription_delete = None;
                    return;
                }
                _ => {
                    self.ui_state.subscription_delete = None;
                    return;
                }
            }
        }
        if self.ui_state.page == Page::Dashboard {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.ui_state.dashboard_scroll = self
                        .ui_state
                        .dashboard_scroll
                        .saturating_add(1)
                        .min(self.ui_state.dashboard_max_scroll);
                    return;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.ui_state.dashboard_scroll =
                        self.ui_state.dashboard_scroll.saturating_sub(1);
                    return;
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    self.ui_state.dashboard_scroll = 0;
                    return;
                }
                KeyCode::Char('G') | KeyCode::End => {
                    self.ui_state.dashboard_scroll = self.ui_state.dashboard_max_scroll;
                    return;
                }
                _ => {}
            }
        }
        if self.ui_state.page == Page::Help {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.ui_state.help_scroll = self
                        .ui_state
                        .help_scroll
                        .saturating_add(1)
                        .min(self.ui_state.help_max_scroll);
                    return;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.ui_state.help_scroll = self.ui_state.help_scroll.saturating_sub(1);
                    return;
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    self.ui_state.help_scroll = 0;
                    return;
                }
                KeyCode::Char('G') | KeyCode::End => {
                    self.ui_state.help_scroll = self.ui_state.help_max_scroll;
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('q') => self.detach_clients(),
            KeyCode::Esc => {
                if self.ui_state.page == Page::Help {
                    self.return_from_help();
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                let target = if MAIN_PAGES.contains(&self.ui_state.page) {
                    self.ui_state.page.next_main()
                } else {
                    Page::Dashboard
                };
                self.switch_page(target);
            }
            KeyCode::Char('h') | KeyCode::Left => {
                let target = if MAIN_PAGES.contains(&self.ui_state.page) {
                    self.ui_state.page.previous_main()
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
            KeyCode::Char('d') if self.ui_state.page == Page::Subscriptions => {
                self.request_delete_subscription();
            }
            KeyCode::Char('n') if self.ui_state.page == Page::Subscriptions => {
                self.ui_state.overlay = Some(Overlay::Edit(EditForm::Subscription(
                    SubscriptionForm::new(),
                )));
                self.ui_state.message = None;
            }
            KeyCode::Char('e') if self.ui_state.page == Page::Subscriptions => {
                self.open_edit_subscription_form();
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.jump_selection(false),
            KeyCode::Char('G') | KeyCode::End => self.jump_selection(true),
            KeyCode::Char('f') if self.ui_state.page == Page::Logs => self.toggle_log_follow(),
            KeyCode::Char('d') if self.ui_state.page == Page::Logs => self.cycle_log_timestamp(),
            KeyCode::Char('v') if self.ui_state.page == Page::Logs => self.cycle_log_filter(),
            KeyCode::Char('x') if self.ui_state.page == Page::Logs => self.cycle_log_source(),
            KeyCode::Char('c') if self.ui_state.page == Page::Logs => {
                self.logger.clear_view();
                self.ui_state.selected = 0;
                self.ui_state.log_follow = true;
            }
            KeyCode::Char('a') if self.ui_state.page == Page::Nodes => self.select_node_auto(),
            KeyCode::Char('n') if self.ui_state.page == Page::Nodes => self.toggle_node_sort(),
            KeyCode::Char('t') if self.ui_state.page == Page::Nodes => self.test_node_delay(),
            KeyCode::Enter => self.handle_enter().await,
            _ => {}
        }
    }

    pub(super) async fn handle_enter(&mut self) {
        match self.ui_state.page {
            Page::Dashboard => {
                if self.ui_state.last_error.is_some() {
                    self.switch_page(Page::Logs);
                }
            }
            Page::Nodes => self.select_node().await,
            Page::Subscriptions => self.open_edit_subscription_form(),
            Page::Connections => self.copy_connection_target(),
            Page::Logs => self.open_log_detail(),
            Page::Settings => self.activate_setting(),
            _ => {}
        }
    }

    pub(super) fn toggle_help(&mut self) {
        if self.ui_state.page == Page::Help {
            self.return_from_help();
            return;
        }
        self.ui_state.help_return = Some(self.ui_state.page);
        self.ui_state.help_scroll = 0;
        self.switch_page(Page::Help);
    }

    pub(super) fn return_from_help(&mut self) {
        let target = self.ui_state.help_return.take().unwrap_or(Page::Dashboard);
        self.ui_state.help_scroll = 0;
        self.switch_page(target);
    }

    pub(super) fn open_setup_overlay(&mut self) {
        self.close_overlay();
        self.ui_state.overlay = Some(Overlay::Setup { selected: 0 });
        self.setup.pending_install = None;
        self.setup.candidates = self.binary_manager.scan_inbox().unwrap_or_default();
        self.setup.message = None;
        self.refresh_previous_binary_state();
        self.maybe_check_binary_update();
    }

    pub(super) fn copy_binary_path(&mut self) {
        let path = self.paths.binary().display().to_string();
        self.broadcast(ServerMsg::Copy { text: path });
        self.notify(self.text().settings_copied);
    }

    pub(super) fn copy_connection_target(&mut self) {
        let selected = self
            .ui_state
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

    pub(super) fn open_log_detail(&mut self) {
        if let Some(line) = self.log_lines().get(self.ui_state.selected).cloned() {
            self.ui_state.overlay = Some(Overlay::LogDetail { line, scroll: 0 });
            self.ui_state.log_detail_max_scroll = 0;
        }
    }

    pub(super) fn detach_clients(&mut self) {
        self.broadcast(ServerMsg::Detach);
        self.clients.retain(|_, client| !client.viewer);
        // `ClientDetached` for those ids arrives after the map entries are
        // gone, so it cannot clear the flag; do it here.
        self.viewer_active.store(false, Ordering::Relaxed);
    }

    pub(super) fn switch_page(&mut self, page: Page) {
        if self.ui_state.page == page {
            return;
        }
        self.ui_state.page_selection[self.ui_state.page.index()] = self.ui_state.selected;
        if self.ui_state.page == Page::Help && page != Page::Help {
            self.ui_state.help_return = None;
        }
        self.close_overlay();
        self.ui_state.page = page;
        self.ui_state.subscription_delete = None;
        self.ui_state.rollback_confirm = false;
        let max = self.max_selection_for(page);
        if page == Page::Logs && self.ui_state.log_follow {
            self.ui_state.selected = max.saturating_sub(1);
        } else {
            self.ui_state.selected =
                self.ui_state.page_selection[page.index()].min(max.saturating_sub(1));
        }
        if page == Page::Nodes {
            self.sort_nodes_preserving_selection();
            self.maybe_test_node_delays(false);
        }
        if page == Page::Settings {
            self.maybe_check_binary_update();
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        if self.ui_state.page == Page::Logs {
            self.ui_state.log_follow = false;
        }
        let max = self.max_selection();
        if max == 0 {
            self.ui_state.selected = 0;
            return;
        }
        let next = self.ui_state.selected as i32 + delta;
        self.ui_state.selected = next.clamp(0, max as i32 - 1) as usize;
        if self.ui_state.page == Page::Logs && self.ui_state.selected == max - 1 {
            self.ui_state.log_follow = true;
        }
    }

    pub(super) fn jump_selection(&mut self, bottom: bool) {
        if self.ui_state.page == Page::Logs {
            self.ui_state.log_follow = bottom;
        }
        let max = self.max_selection();
        self.ui_state.selected = if bottom { max.saturating_sub(1) } else { 0 };
    }

    pub(super) fn max_selection(&self) -> usize {
        self.max_selection_for(self.ui_state.page)
    }

    pub(super) fn max_selection_for(&self, page: Page) -> usize {
        match page {
            Page::Nodes => self.nodes.len(),
            Page::Connections => self.metrics.connections.len(),
            Page::Subscriptions => self.subscription_state.subscriptions.len(),
            Page::Logs => self.log_count(),
            Page::Settings => settings_items(self.config.allow_lan).len(),
            _ => 0,
        }
    }

    pub(super) fn toggle_node_sort(&mut self) {
        self.ui_state.node_sort = match self.ui_state.node_sort {
            NodeSort::AutoLatency => NodeSort::Name,
            NodeSort::Name => NodeSort::AutoLatency,
        };
        self.sort_nodes_preserving_selection();
    }

    pub(super) fn sort_nodes_if_visible(&mut self) {
        if !(self.has_viewer() && self.ui_state.page == Page::Nodes) {
            return;
        }
        if self
            .ui_state
            .last_node_sort
            .is_some_and(|at| at.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        self.sort_nodes_preserving_selection();
    }

    pub(super) fn sort_nodes_preserving_selection(&mut self) {
        self.refresh_auto_tags_cache();
        let selected_tag = self
            .nodes
            .get(self.ui_state.selected)
            .map(|node| node.tag.clone());
        let auto_tags: HashSet<String> = self.auto_candidate_tags().into_iter().collect();
        sort_nodes(
            &mut self.nodes,
            self.ui_state.node_sort,
            &auto_tags,
            &self.node_delays,
        );
        self.ui_state.selected = selected_tag
            .and_then(|tag| self.nodes.iter().position(|node| node.tag == tag))
            .unwrap_or_else(|| {
                self.ui_state
                    .selected
                    .min(self.nodes.len().saturating_sub(1))
            });
        // Sorting changes the node order, and the cache signature includes it.
        self.refresh_auto_tags_cache();
        self.ui_state.last_node_sort = Some(Instant::now());
    }

    pub fn log_lines(&self) -> Vec<LogLine> {
        self.logger
            .recent_filtered(500, self.ui_state.log_filter, self.ui_state.log_source)
    }

    pub(super) fn log_count(&self) -> usize {
        self.logger
            .count_recent(500, self.ui_state.log_filter, self.ui_state.log_source)
    }

    pub(super) fn toggle_log_follow(&mut self) {
        self.ui_state.log_follow = !self.ui_state.log_follow;
        if self.ui_state.log_follow {
            self.ui_state.selected = self.max_selection().saturating_sub(1);
        }
        let state = if self.ui_state.log_follow {
            self.text().common_on
        } else {
            self.text().common_off
        };
        self.notify(format!("{}: {state}", self.text().logs_follow));
    }

    pub(super) fn cycle_log_timestamp(&mut self) {
        self.ui_state.log_timestamp = self.ui_state.log_timestamp.next();
    }

    pub(super) fn cycle_log_filter(&mut self) {
        self.ui_state.log_filter = self.ui_state.log_filter.next_filter();
        self.reset_log_selection();
        self.notify(format!(
            "{}: {}",
            self.text().logs_level,
            self.ui_state.log_filter.label()
        ));
    }

    pub(super) fn cycle_log_source(&mut self) {
        self.ui_state.log_source = self.ui_state.log_source.next();
        self.reset_log_selection();
        self.notify(format!(
            "{}: {}",
            self.text().logs_source,
            self.ui_state.log_source.label()
        ));
    }

    pub(super) fn reset_log_selection(&mut self) {
        let max = self.max_selection();
        if self.ui_state.log_follow {
            self.ui_state.selected = max.saturating_sub(1);
        } else {
            self.ui_state.selected = self.ui_state.selected.min(max.saturating_sub(1));
        }
    }

    pub(super) async fn refresh_current(&mut self) {
        match self.ui_state.page {
            Page::Nodes => self.maybe_test_node_delays(true),
            Page::Subscriptions | Page::Dashboard => self.refresh_subscriptions(),
            _ => {
                self.local_ips = netinfo::local_addresses(false);
                self.last_local_ips = Instant::now();
            }
        }
        self.refresh_proxy_ips();
    }

    pub(super) fn open_edit_subscription_form(&mut self) {
        let Some(runtime) = self
            .subscription_state
            .subscriptions
            .get(self.ui_state.selected)
        else {
            self.notify(self.text().subscriptions_empty);
            return;
        };
        self.ui_state.overlay = Some(Overlay::Edit(EditForm::Subscription(
            SubscriptionForm::edit(&runtime.config),
        )));
        self.ui_state.message = None;
    }

    pub(super) fn request_delete_subscription(&mut self) {
        let Some(runtime) = self
            .subscription_state
            .subscriptions
            .get(self.ui_state.selected)
        else {
            return;
        };
        self.ui_state.subscription_delete = Some(runtime.config.id.clone());
        self.ui_state.message = None;
        self.ui_state.message_at = None;
    }

    pub(super) fn delete_subscription(&mut self, id: &str) {
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
            self.ui_state.last_error = Some(err.to_string());
            return;
        }
        self.config = config;
        self.subscription_state.subscription_refresh_seq.remove(id);
        if self
            .subscription_state
            .pending_restart_subscription
            .as_deref()
            == Some(id)
        {
            self.subscription_state.pending_restart_subscription = None;
        }
        self.subscription_state
            .subscriptions
            .retain(|runtime| runtime.config.id != id);
        self.nodes = merge_nodes(&self.subscription_state.subscriptions);
        self.ui_state.last_node_sort = None;
        self.prune_node_state();
        self.sync_preferred_node();
        self.ui_state.selected = self.ui_state.selected.min(
            self.subscription_state
                .subscriptions
                .len()
                .saturating_sub(1),
        );
        self.save_subscription_cache();
        self.refresh_last_error_from_subscriptions();
        self.notify(self.text().msg_subscription_deleted);
        if was_enabled {
            self.apply_nodes_change_to_proxy();
        }
        self.logger
            .app(LogLevel::Info, format!("subscription deleted: {id}"));
    }

    pub(super) async fn handle_overlay_key(&mut self, kind: OverlayKind, key: KeyEvent) {
        match kind {
            OverlayKind::Subscription => self.handle_form_key(key),
            OverlayKind::Keyword => self.handle_keyword_form_key(key),
            OverlayKind::Settings => self.handle_settings_form_key(key),
            OverlayKind::Setup => self.handle_setup_overlay_key(key).await,
            OverlayKind::LogDetail => self.handle_log_detail_key(key),
        }
    }

    pub(super) fn setup_overlay_selected(&self) -> Option<usize> {
        match &self.ui_state.overlay {
            Some(Overlay::Setup { selected }) => Some(*selected),
            _ => None,
        }
    }

    pub(super) fn move_setup_selection(&mut self, delta: i32) {
        let count = self.setup.candidates.len();
        if let Some(Overlay::Setup { selected }) = self.ui_state.overlay.as_mut() {
            if count == 0 {
                *selected = 0;
                return;
            }
            let next = *selected as i32 + delta;
            *selected = next.clamp(0, count as i32 - 1) as usize;
        }
    }

    pub(super) fn jump_setup_selection(&mut self, bottom: bool) {
        let count = self.setup.candidates.len();
        if let Some(Overlay::Setup { selected }) = self.ui_state.overlay.as_mut() {
            *selected = if bottom { count.saturating_sub(1) } else { 0 };
        }
    }

    pub(super) fn rescan_inbox(&mut self) {
        self.setup.candidates = self.binary_manager.scan_inbox().unwrap_or_default();
        if let Some(Overlay::Setup { selected }) = self.ui_state.overlay.as_mut() {
            *selected = 0;
        }
        self.setup.message = None;
    }

    pub(super) async fn handle_setup_overlay_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_overlay(),
            KeyCode::Char('j') | KeyCode::Down => self.move_setup_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_setup_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.jump_setup_selection(false),
            KeyCode::Char('G') | KeyCode::End => self.jump_setup_selection(true),
            KeyCode::Enter => self.install_selected().await,
            KeyCode::Char('d') => self.start_download(),
            KeyCode::Char('b') => self.request_rollback(),
            KeyCode::Char('p') => self.copy_binary_path(),
            KeyCode::Char('r') => self.rescan_inbox(),
            _ => {}
        }
    }

    pub(super) fn handle_log_detail_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.close_overlay(),
            KeyCode::Char('j') | KeyCode::Down => self.scroll_log_detail(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_log_detail(-1),
            KeyCode::Char('g') | KeyCode::Home => self.scroll_log_detail_to(0),
            KeyCode::Char('G') | KeyCode::End => {
                self.scroll_log_detail_to(self.ui_state.log_detail_max_scroll)
            }
            _ => {}
        }
    }

    pub(super) fn scroll_log_detail(&mut self, delta: i32) {
        let max = self.ui_state.log_detail_max_scroll;
        if let Some(Overlay::LogDetail { scroll, .. }) = self.ui_state.overlay.as_mut() {
            *scroll = if delta >= 0 {
                scroll.saturating_add(delta as usize).min(max)
            } else {
                scroll.saturating_sub((-delta) as usize)
            };
        }
    }

    pub(super) fn scroll_log_detail_to(&mut self, offset: usize) {
        if let Some(Overlay::LogDetail { scroll, .. }) = self.ui_state.overlay.as_mut() {
            *scroll = offset.min(self.ui_state.log_detail_max_scroll);
        }
    }

    pub(super) fn handle_form_key(&mut self, key: KeyEvent) {
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

    pub(super) fn save_subscription_form(&mut self) {
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
                .subscription_state
                .subscriptions
                .iter()
                .any(|runtime| runtime.config.id == config.id)
            {
                self.subscription_state
                    .subscriptions
                    .push(empty_runtime(config.clone()));
            }
            if restart_needed {
                self.subscription_state.pending_restart_subscription = Some(config.id.clone());
            }
            self.refresh_subscription(config);
        } else {
            self.subscription_state
                .subscription_refresh_seq
                .remove(&config.id);
            let placeholder = empty_runtime(config);
            if let Some(existing) = self
                .subscription_state
                .subscriptions
                .iter_mut()
                .find(|runtime| runtime.config.id == placeholder.config.id)
            {
                *existing = placeholder;
            } else {
                self.subscription_state.subscriptions.push(placeholder);
            }
            self.nodes = merge_nodes(&self.subscription_state.subscriptions);
            self.ui_state.last_node_sort = None;
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
    pub(super) fn apply_nodes_change_to_proxy(&mut self) {
        if self.nodes.is_empty() {
            if self.proxy_runtime.proxy.is_some() {
                self.stop_proxy();
            }
        } else {
            self.auto_restart_after_change();
        }
    }

    pub(super) fn open_keyword_form(&mut self, field: KeywordField) {
        self.ui_state.overlay = Some(Overlay::Edit(EditForm::Keyword(KeywordForm::from_config(
            &self.config,
            field,
        ))));
        self.ui_state.message = None;
    }

    pub(super) fn activate_setting(&mut self) {
        let items = settings_items(self.config.allow_lan);
        let Some(item) = items.get(self.ui_state.selected).copied() else {
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
            SettingsItem::BinaryInfo => self.open_setup_overlay(),
            SettingsItem::Port
            | SettingsItem::DnsServers
            | SettingsItem::TestUrl
            | SettingsItem::ConfigPath
            | SettingsItem::LogFile => {}
        }
    }

    pub(super) fn setting_value(&self, item: SettingsItem) -> String {
        match item {
            SettingsItem::Port => self.config.port.to_string(),
            SettingsItem::DnsServers => self.config.dns_servers.join(", "),
            SettingsItem::TestUrl => self.config.test_url.clone(),
            SettingsItem::ConfigPath => self.paths.config_file.display().to_string(),
            SettingsItem::LogFile => self.logger.current_file().display().to_string(),
            _ => String::new(),
        }
    }

    pub(super) fn open_settings_form(&mut self) {
        self.ui_state.overlay = Some(Overlay::Edit(EditForm::Settings(SettingsForm::new(
            &self.config,
        ))));
        self.ui_state.message = None;
    }

    pub(super) fn handle_settings_form_key(&mut self, key: KeyEvent) {
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

    pub(super) fn save_settings_form(&mut self) {
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

    pub(super) fn cycle_app_log_level(&mut self) {
        let mut config = self.config.clone();
        config.app_log_level = config.app_log_level.next_filter();
        if let Err(err) = config.save(&self.paths.config_file) {
            self.ui_state.last_error = Some(err.to_string());
            return;
        }
        self.config = config;
        self.logger
            .set_levels(self.config.app_log_level, self.config.singbox_log_level);
        self.notify(self.text().settings_saved);
    }

    pub(super) fn notify_saved_with_restart(&mut self) {
        self.notify(self.text().settings_saved);
        self.auto_restart_after_change();
    }

    pub(super) fn handle_keyword_form_key(&mut self, key: KeyEvent) {
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

    pub(super) fn save_keyword_form(&mut self) {
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
        let filtered = self.auto_candidate_tags().len();
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
}
