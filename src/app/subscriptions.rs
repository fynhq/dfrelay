use super::*;

#[derive(Debug, Default)]
pub(crate) struct SubscriptionState {
    pub(crate) subscriptions: Vec<SubscriptionRuntime>,
    pub(crate) refreshing_subscriptions: bool,
    pub(crate) subscription_seq: u64,
    pub(crate) subscription_refresh_seq: HashMap<String, u64>,
    pub(crate) last_subscription_refresh: Option<Instant>,
    pub(crate) subscription_refresh_retry_at: Option<Instant>,
    pub(crate) subscription_refresh_backoff: Duration,
    pub(crate) pending_restart_subscription: Option<String>,
    pub(crate) pending_refresh_apply: Option<PendingRefreshApply>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingRefreshApply {
    pub(crate) diff: SubscriptionDiff,
    pub(crate) since: Instant,
    pub(crate) wait_logged: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SubscriptionDiff {
    pub(crate) added: usize,
    pub(crate) removed: usize,
    pub(crate) auto_changed: bool,
}

impl SubscriptionDiff {
    pub(crate) fn needs_rebuild(self) -> bool {
        self.added > 0 || self.removed > 0 || self.auto_changed
    }

    pub(crate) fn detail(self) -> String {
        let auto = if self.auto_changed {
            ", auto-candidates changed"
        } else {
            ""
        };
        format!("+{} -{}{auto}", self.added, self.removed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SubscriptionApplyReason {
    Idle,
    HealthDegraded,
    MaxDefer,
}

impl SubscriptionApplyReason {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::HealthDegraded => "health-degraded",
            Self::MaxDefer => "max-defer",
        }
    }
}

pub(super) fn subscription_apply_reason(
    idle: bool,
    degraded: bool,
    pending_for: Duration,
) -> Option<SubscriptionApplyReason> {
    if idle {
        Some(SubscriptionApplyReason::Idle)
    } else if degraded {
        Some(SubscriptionApplyReason::HealthDegraded)
    } else if pending_for >= REFRESH_APPLY_MAX_DEFER {
        Some(SubscriptionApplyReason::MaxDefer)
    } else {
        None
    }
}

impl App {
    pub(super) fn refresh_subscriptions(&mut self) {
        if self.subscription_state.refreshing_subscriptions {
            return;
        }
        let configs: Vec<SubscriptionConfig> =
            self.config.enabled_subscriptions().cloned().collect();
        if configs.is_empty() {
            self.ui_state.last_error = Some(self.text().msg_no_enabled_subscriptions.to_string());
            return;
        }
        self.subscription_state.refreshing_subscriptions = true;
        let seq = self.next_subscription_seq();
        for config in &configs {
            self.subscription_state
                .subscription_refresh_seq
                .insert(config.id.clone(), seq);
        }
        let previous = self.subscription_state.subscriptions.clone();
        let tx = self.events_tx.clone();
        let logger = self.logger.clone();
        self.spawn_guarded(BackgroundTask::SubscriptionRefresh, async move {
            logger.app(
                LogLevel::Info,
                format!("refreshing {} subscriptions", configs.len()),
            );
            let runtimes =
                subscription::refresh_all(&configs, &FetchOptions::default(), &previous).await;
            let _ = tx.send(AppEvent::Subscriptions { seq, runtimes });
        });
    }

    pub(super) fn subscription_refresh_due(&self) -> bool {
        if self.subscription_state.refreshing_subscriptions
            || self.config.subscription_refresh_hours == 0
            || self.config.enabled_subscriptions().next().is_none()
        {
            return false;
        }
        let now = Instant::now();
        if self
            .subscription_state
            .subscription_refresh_retry_at
            .is_some_and(|at| at > now)
        {
            return false;
        }
        let interval =
            Duration::from_secs(self.config.subscription_refresh_hours.saturating_mul(3600));
        self.subscription_state
            .last_subscription_refresh
            .is_none_or(|last| last.elapsed() >= interval)
    }

    pub(super) fn subscription_refresh_succeeded(&mut self) {
        self.subscription_state.last_subscription_refresh = Some(Instant::now());
        self.subscription_state.subscription_refresh_retry_at = None;
        self.subscription_state.subscription_refresh_backoff = SUBSCRIPTION_REFRESH_MIN_BACKOFF;
    }

    pub(super) fn clear_refresh_retry_if_healthy(&mut self) {
        let has_errors = self
            .subscription_state
            .subscriptions
            .iter()
            .any(|runtime| runtime.config.enabled && runtime.error.is_some());
        if !has_errors {
            self.subscription_state.subscription_refresh_retry_at = None;
            self.subscription_state.subscription_refresh_backoff = SUBSCRIPTION_REFRESH_MIN_BACKOFF;
        }
    }

    pub(super) fn schedule_subscription_retry(&mut self) {
        self.subscription_state.subscription_refresh_retry_at =
            Some(Instant::now() + self.subscription_state.subscription_refresh_backoff);
        self.subscription_state.subscription_refresh_backoff =
            (self.subscription_state.subscription_refresh_backoff * 2)
                .min(SUBSCRIPTION_REFRESH_MAX_BACKOFF);
    }

    pub(super) fn subscription_tag_diff(&self) -> SubscriptionDiff {
        if self.proxy_runtime.proxy_node_tags.is_empty() {
            return SubscriptionDiff::default();
        }
        let live: HashSet<&str> = self
            .proxy_runtime
            .proxy_node_tags
            .iter()
            .map(String::as_str)
            .collect();
        let next: HashSet<&str> = self
            .metrics_state
            .nodes
            .iter()
            .map(|node| node.tag.as_str())
            .collect();
        let auto_now: HashSet<String> = self.auto_candidate_tags().into_iter().collect();
        SubscriptionDiff {
            added: next.difference(&live).count(),
            removed: live.difference(&next).count(),
            auto_changed: self.proxy_runtime.proxy_auto_tags != auto_now,
        }
    }

    pub(super) fn note_subscription_apply(&mut self) {
        if self.proxy_runtime.proxy_node_tags.is_empty() {
            self.subscription_state.pending_refresh_apply = None;
            return;
        }
        let diff = self.subscription_tag_diff();
        if !diff.needs_rebuild() {
            if self
                .subscription_state
                .pending_refresh_apply
                .take()
                .is_some()
            {
                self.logger
                    .app(LogLevel::Info, "subscription apply pending cleared");
            }
            return;
        }
        match self.subscription_state.pending_refresh_apply.as_mut() {
            Some(pending) => {
                pending.diff = diff;
            }
            None => {
                self.subscription_state.pending_refresh_apply = Some(PendingRefreshApply {
                    diff,
                    since: Instant::now(),
                    wait_logged: false,
                });
                self.logger.app(
                    LogLevel::Info,
                    format!("subscription apply pending: {}", diff.detail()),
                );
            }
        }
    }

    pub(super) fn maybe_apply_subscription_changes(&mut self) {
        let Some(pending) = self.subscription_state.pending_refresh_apply.as_ref() else {
            return;
        };
        let (diff, since, wait_logged) = (pending.diff, pending.since, pending.wait_logged);
        if self.proxy_runtime.proxy.is_none() || self.proxy_runtime.status != ProxyStatus::Running {
            self.subscription_state.pending_refresh_apply = None;
            self.logger.app(
                LogLevel::Info,
                "subscription apply pending cleared: proxy not running",
            );
            return;
        }
        if self.subscription_state.refreshing_subscriptions
            || self.proxy_runtime.restart_in_progress
            || self.proxy_runtime.preflight_in_flight
            || self.health.recovery_in_flight
            || self
                .subscription_state
                .pending_restart_subscription
                .is_some()
            || self.binary_state.setup.downloading
            || self.binary_state.setup.rollback_in_flight
            || self.binary_state.install_in_flight
        {
            return;
        }
        let idle = self.metrics_api_fresh()
            && self
                .metrics_state
                .idle_since
                .is_some_and(|at| at.elapsed() >= REFRESH_IDLE_GRACE);
        let degraded = self
            .health
            .last_failure_at
            .is_some_and(|at| at.elapsed() <= REFRESH_DEGRADED_WINDOW)
            && self
                .health
                .last_failure_tag
                .as_deref()
                .is_some_and(|tag| self.health_target().as_deref() == Some(tag));
        let Some(reason) = subscription_apply_reason(idle, degraded, since.elapsed()) else {
            if !wait_logged && since.elapsed() >= REFRESH_IDLE_WAIT_LOG {
                if let Some(pending) = self.subscription_state.pending_refresh_apply.as_mut() {
                    pending.wait_logged = true;
                }
                self.logger.app(
                    LogLevel::Info,
                    format!(
                        "subscription changes pending for {}s: waiting for idle or health degradation (max {}m) ({})",
                        since.elapsed().as_secs(),
                        REFRESH_APPLY_MAX_DEFER.as_secs() / 60,
                        diff.detail()
                    ),
                );
            }
            return;
        };
        if self.auto_restart_after_change() {
            self.subscription_state.pending_refresh_apply = None;
            self.logger.app(
                LogLevel::Info,
                format!(
                    "subscription changes applying ({}): {}",
                    reason.label(),
                    diff.detail()
                ),
            );
            if reason == SubscriptionApplyReason::Idle {
                self.notify(self.text().msg_subscription_applied_idle);
            }
        } else {
            self.subscription_state.pending_refresh_apply = None;
            self.logger.app(
                LogLevel::Warn,
                format!(
                    "subscription apply skipped ({}): {}",
                    reason.label(),
                    diff.detail()
                ),
            );
        }
    }

    pub(super) fn reconcile_subscription_apply_after_start(&mut self) {
        let diff = self.subscription_tag_diff();
        if !diff.needs_rebuild() {
            if self
                .subscription_state
                .pending_refresh_apply
                .take()
                .is_some()
            {
                self.logger.app(
                    LogLevel::Info,
                    "subscription apply pending resolved by restart",
                );
            }
            return;
        }
        self.subscription_state.pending_refresh_apply = Some(PendingRefreshApply {
            diff,
            since: Instant::now(),
            wait_logged: false,
        });
        self.logger.app(
            LogLevel::Info,
            format!(
                "subscription apply still pending after start: {}",
                diff.detail()
            ),
        );
    }

    pub(super) fn restart_after_pending_subscription(&mut self) {
        let Some(id) = self.subscription_state.pending_restart_subscription.take() else {
            return;
        };
        let diff = self.subscription_tag_diff();
        if !diff.needs_rebuild() {
            self.logger.app(
                LogLevel::Info,
                format!("subscription {id} refresh produced no config changes; restart skipped"),
            );
            return;
        }
        self.logger.app(
            LogLevel::Info,
            format!("subscription {id} changed: {}; restarting", diff.detail()),
        );
        if !self.auto_restart_after_change() {
            self.logger.app(
                LogLevel::Info,
                "subscription restart deferred until next start",
            );
        }
    }

    pub(super) fn after_subscription_update(&mut self, id: &str) {
        if self
            .subscription_state
            .pending_restart_subscription
            .as_deref()
            == Some(id)
        {
            self.restart_after_pending_subscription();
        } else {
            self.note_subscription_apply();
        }
        self.retry_pending_start();
    }

    pub(super) fn recompute_nodes(&mut self) {
        let selected_tag = if self.ui_state.page == Page::Nodes {
            self.metrics_state
                .nodes
                .get(self.ui_state.selected)
                .map(|node| node.tag.clone())
        } else {
            None
        };
        self.metrics_state.nodes = merge_nodes(&self.subscription_state.subscriptions);
        self.prune_node_state();
        self.sync_preferred_node();
        if let Some(tag) = selected_tag {
            if let Some(index) = self
                .metrics_state
                .nodes
                .iter()
                .position(|node| node.tag == tag)
            {
                self.ui_state.selected = index;
            }
        }
        self.ui_state.selected = self
            .ui_state
            .selected
            .min(self.max_selection_for(self.ui_state.page).saturating_sub(1));
        if self.ui_state.page == Page::Nodes {
            self.ui_state.nodes_cursor_tag = self
                .metrics_state
                .nodes
                .get(self.ui_state.selected)
                .map(|node| node.tag.clone());
        }
        self.ui_state.last_node_sort = None;
        self.sort_nodes_if_visible();
    }

    pub(super) fn finalize_subscription_change(&mut self) {
        self.save_subscription_cache();
        self.refresh_last_error_from_subscriptions();
    }

    pub(super) fn save_subscription_cache(&self) {
        if let Err(err) = subscription::cache::save(
            &self.paths.subscription_cache(),
            &self.subscription_state.subscriptions,
        ) {
            self.logger.app(
                LogLevel::Warn,
                format!("write subscription cache failed: {err}"),
            );
        }
    }

    pub(super) fn next_subscription_seq(&mut self) -> u64 {
        self.subscription_state.subscription_seq =
            self.subscription_state.subscription_seq.wrapping_add(1);
        self.subscription_state.subscription_seq
    }

    pub(super) fn refresh_last_error_from_subscriptions(&mut self) {
        let errors: Vec<String> = self
            .subscription_state
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
            self.ui_state.last_error = Some(errors.join("; "));
        } else if self.proxy_runtime.status != ProxyStatus::Failed
            && self.ui_state.last_error.as_deref() != Some(self.text().msg_health_no_nodes)
        {
            self.ui_state.last_error = None;
        }
    }

    pub(super) fn refresh_subscription(&mut self, config: SubscriptionConfig) {
        let previous = self
            .subscription_state
            .subscriptions
            .iter()
            .find(|runtime| runtime.config.id == config.id)
            .cloned();
        let seq = self.next_subscription_seq();
        self.subscription_state
            .subscription_refresh_seq
            .insert(config.id.clone(), seq);
        let tx = self.events_tx.clone();
        let logger = self.logger.clone();
        self.spawn_guarded(BackgroundTask::Other, async move {
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
}
