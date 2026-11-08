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
