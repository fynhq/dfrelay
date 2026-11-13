//! Subscription refresh orchestration.
//!
//! Scheduling and cache writing live here; the form editing and list UI stay
//! in `app.rs`.

use super::*;

impl App {
    pub(super) fn refresh_subscriptions(&mut self) {
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
        let seq = self.next_subscription_seq();
        for config in &configs {
            self.subscription_refresh_seq.insert(config.id.clone(), seq);
        }
        let previous = self.subscriptions.clone();
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
        if self.refreshing_subscriptions
            || self.config.subscription_refresh_hours == 0
            || self.config.enabled_subscriptions().next().is_none()
        {
            return false;
        }
        let now = Instant::now();
        if self
            .subscription_refresh_retry_at
            .is_some_and(|at| at > now)
        {
            return false;
        }
        let interval =
            Duration::from_secs(self.config.subscription_refresh_hours.saturating_mul(3600));
        self.last_subscription_refresh
            .is_none_or(|last| last.elapsed() >= interval)
    }

    pub(super) fn subscription_refresh_succeeded(&mut self) {
        self.last_subscription_refresh = Some(Instant::now());
        self.subscription_refresh_retry_at = None;
        self.subscription_refresh_backoff = SUBSCRIPTION_REFRESH_MIN_BACKOFF;
    }

    pub(super) fn schedule_subscription_retry(&mut self) {
        self.subscription_refresh_retry_at =
            Some(Instant::now() + self.subscription_refresh_backoff);
        self.subscription_refresh_backoff =
            (self.subscription_refresh_backoff * 2).min(SUBSCRIPTION_REFRESH_MAX_BACKOFF);
    }

    pub(super) fn subscription_tag_diff(&self) -> (usize, usize) {
        if self.proxy_node_tags.is_empty() {
            return (0, 0);
        }
        let live: HashSet<&str> = self.proxy_node_tags.iter().map(String::as_str).collect();
        let next: HashSet<&str> = self.nodes.iter().map(|node| node.tag.as_str()).collect();
        (
            next.difference(&live).count(),
            live.difference(&next).count(),
        )
    }

    /// Record that the refreshed node set differs from the running config.
    /// The restart itself waits until the proxy is idle when configured.
    pub(super) fn note_subscription_apply(&mut self) {
        if self.proxy_node_tags.is_empty() {
            self.pending_refresh_apply = None;
            return;
        }
        let (added, removed) = self.subscription_tag_diff();
        if added == 0 && removed == 0 {
            if self.pending_refresh_apply.take().is_some() {
                self.logger
                    .app(LogLevel::Info, "subscription apply pending cleared");
            }
            return;
        }
        match self.pending_refresh_apply.as_mut() {
            Some(pending) => {
                pending.added = added;
                pending.removed = removed;
            }
            None => {
                self.pending_refresh_apply = Some(PendingRefreshApply {
                    added,
                    removed,
                    since: Instant::now(),
                    wait_logged: false,
                });
                self.logger.app(
                    LogLevel::Info,
                    format!("subscription apply pending: +{added} -{removed}"),
                );
            }
        }
    }

    pub(super) fn maybe_apply_subscription_changes(&mut self) {
        let Some(pending) = self.pending_refresh_apply.as_ref() else {
            return;
        };
        let (added, removed, since, wait_logged) = (
            pending.added,
            pending.removed,
            pending.since,
            pending.wait_logged,
        );
        if self.proxy.is_none() || self.status != ProxyStatus::Running {
            self.pending_refresh_apply = None;
            self.logger.app(
                LogLevel::Info,
                "subscription apply pending cleared: proxy not running",
            );
            return;
        }
        if self.refreshing_subscriptions
            || self.restart_in_progress
            || self.preflight_in_flight
            || self.health.recovery_in_flight
            || self.pending_restart_subscription.is_some()
            || self.setup.downloading
            || self.setup.rollback_in_flight
            || self.install_in_flight
        {
            return;
        }
        if self
            .health
            .last_metrics_at
            .is_none_or(|at| at.elapsed() > HEALTH_PROBE_STALE)
        {
            return;
        }
        let idle = self
            .idle_since
            .is_some_and(|at| at.elapsed() >= REFRESH_IDLE_GRACE);
        if !idle {
            if !wait_logged && since.elapsed() >= REFRESH_IDLE_WAIT_LOG {
                if let Some(pending) = self.pending_refresh_apply.as_mut() {
                    pending.wait_logged = true;
                }
                self.logger.app(
                    LogLevel::Info,
                    format!(
                        "subscription changes pending for {}s: waiting for idle (+{added} -{removed})",
                        since.elapsed().as_secs()
                    ),
                );
            }
            return;
        }
        if self.auto_restart_after_change() {
            self.pending_refresh_apply = None;
            self.logger.app(
                LogLevel::Info,
                format!("subscription changes applying while idle: +{added} -{removed}"),
            );
            self.notify(self.text().msg_subscription_applied_idle);
        } else {
            self.pending_refresh_apply = None;
            self.logger.app(
                LogLevel::Warn,
                format!("subscription idle apply skipped: +{added} -{removed}"),
            );
        }
    }

    pub(super) fn reconcile_subscription_apply_after_start(&mut self) {
        let (added, removed) = self.subscription_tag_diff();
        if added == 0 && removed == 0 {
            if self.pending_refresh_apply.take().is_some() {
                self.logger.app(
                    LogLevel::Info,
                    "subscription apply pending resolved by restart",
                );
            }
            return;
        }
        self.pending_refresh_apply = Some(PendingRefreshApply {
            added,
            removed,
            since: Instant::now(),
            wait_logged: false,
        });
        self.logger.app(
            LogLevel::Info,
            format!("subscription apply still pending after start: +{added} -{removed}"),
        );
    }

    pub(super) fn restart_after_pending_subscription(&mut self) {
        let Some(id) = self.pending_restart_subscription.take() else {
            return;
        };
        let (added, removed) = self.subscription_tag_diff();
        if added == 0 && removed == 0 {
            self.logger.app(
                LogLevel::Info,
                format!("subscription {id} refresh produced no node changes; restart skipped"),
            );
            return;
        }
        self.logger.app(
            LogLevel::Info,
            format!("subscription {id} changed: +{added} -{removed}; restarting"),
        );
        if !self.auto_restart_after_change() {
            self.logger.app(
                LogLevel::Info,
                "subscription restart deferred until next start",
            );
        }
    }

    /// Rebuild the merged node list after `self.subscriptions` changed.
    pub(super) fn refresh_merged_nodes(&mut self) {
        self.nodes = merge_nodes(&self.subscriptions);
        self.prune_node_state();
        self.sync_preferred_node();
    }

    /// Shared tail for full and single-subscription refresh results.
    pub(super) fn finalize_subscription_change(&mut self) {
        self.last_node_sort = None;
        self.sort_nodes_if_visible();
        self.save_subscription_cache();
        self.refresh_last_error_from_subscriptions();
    }

    pub(super) fn save_subscription_cache(&self) {
        if let Err(err) =
            subscription::cache::save(&self.paths.subscription_cache(), &self.subscriptions)
        {
            self.logger.app(
                LogLevel::Warn,
                format!("write subscription cache failed: {err}"),
            );
        }
    }

    pub(super) fn next_subscription_seq(&mut self) -> u64 {
        self.subscription_seq = self.subscription_seq.wrapping_add(1);
        self.subscription_seq
    }

    pub(super) fn refresh_last_error_from_subscriptions(&mut self) {
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

    pub(super) fn refresh_subscription(&mut self, config: SubscriptionConfig) {
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
