//! Health probing and failover for the active node.
//!
//! Method group extracted from `app.rs`; `App` itself and its fields stay in
//! the parent module, so this file only adds an `impl App` block.

use super::*;
use crate::singbox::tags;

impl App {
    pub(super) fn health_target(&self) -> Option<String> {
        let selected = self.metrics.selection.selected.as_deref()?;
        let tag = if selected == tags::AUTO {
            self.metrics
                .selection
                .auto_now
                .as_deref()
                .filter(|tag| *tag != tags::AUTO)?
        } else {
            selected
        };
        self.proxy_node_tags
            .iter()
            .any(|item| item == tag)
            .then(|| tag.to_string())
    }

    pub(super) fn maybe_probe_health(&mut self) {
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
        let Some(tag) = self.health_target() else {
            return;
        };
        let active = self
            .last_health_activity
            .is_some_and(|at| now.duration_since(at) <= HEALTH_ACTIVE_WINDOW);
        let confirming =
            self.health_failures > 0 && self.health_failure_tag.as_deref() == Some(tag.as_str());
        let interval = if confirming {
            HEALTH_PROBE_INTERVAL
        } else if self.health_fast_probe_pending {
            HEALTH_FAST_PROBE_GAP
        } else if active {
            HEALTH_PROBE_INTERVAL
        } else {
            HEALTH_IDLE_PROBE_INTERVAL
        };
        if self
            .last_health_probe
            .is_some_and(|at| now.duration_since(at) < interval)
        {
            return;
        }
        let Some(proxy) = self.proxy.as_ref() else {
            return;
        };
        let client = ClashClient::new(proxy.clash.port, proxy.clash.secret.clone());
        let targets = self.probe_targets();
        let tx = self.events_tx.clone();
        let logger = self.logger.clone();
        let generation = self.health_generation;
        self.health_probe_in_flight = true;
        self.health_fast_probe_pending = false;
        self.last_health_probe = Some(now);
        self.spawn_guarded(BackgroundTask::HealthProbe, async move {
            let outcome = probe_node_outcome(&client, &tag, &targets).await;
            if matches!(outcome, HealthOutcome::Unavailable) {
                logger.app(
                    LogLevel::Debug,
                    format!("health probe unavailable for {tag}: no target produced a result"),
                );
            }
            let _ = tx.send(AppEvent::HealthProbe {
                generation,
                tag,
                outcome,
            });
        });
    }

    pub(super) fn reset_health_failures(&mut self) {
        self.health_failures = 0;
        self.health_failure_tag = None;
    }

    /// Probe targets in priority order. The configured URL stays first; the
    /// fallback only runs when the primary fails, so a healthy probe costs one
    /// request and a target-specific block does not fail the node.
    pub(super) fn probe_targets(&self) -> Vec<String> {
        let mut targets = vec![self.config.test_url.clone()];
        if !targets.iter().any(|target| target == FALLBACK_TEST_URL) {
            targets.push(FALLBACK_TEST_URL.to_string());
        }
        targets
    }

    pub(super) fn schedule_no_node_pause(&mut self) {
        let index = (self.health_no_node_level as usize)
            .min(HEALTH_NO_NODE_BACKOFF_SECS.len().saturating_sub(1));
        self.health_pause_until =
            Some(Instant::now() + Duration::from_secs(HEALTH_NO_NODE_BACKOFF_SECS[index]));
        self.health_no_node_level = self
            .health_no_node_level
            .saturating_add(1)
            .min(HEALTH_NO_NODE_BACKOFF_SECS.len() as u8 - 1);
    }

    pub(super) fn invalidate_health_tasks(&mut self) {
        self.health_generation = self.health_generation.wrapping_add(1);
        self.health_probe_in_flight = false;
        self.health_fast_probe_pending = false;
        self.recovery_in_flight = false;
        self.health_pause_until = None;
        self.health_no_node_level = 0;
        self.recovery_refresh_pending = false;
        self.reset_health_failures();
        self.last_recovery = None;
    }

    pub(super) fn reset_health_state(&mut self) {
        self.invalidate_health_tasks();
        self.last_health_probe = None;
        self.last_health_activity = None;
        self.health_connection_ids.clear();
        self.last_metrics_at = None;
        self.recovery_refresh_pending = false;
        self.idle_since = None;
        self.last_logged_selection = None;
        self.pending_switch_reason = None;
    }

    pub(super) fn recovery_candidates(&self, trigger: &str) -> Vec<String> {
        let runtime: HashSet<&str> = self.proxy_node_tags.iter().map(String::as_str).collect();
        let auto_mode = self.metrics.selection.selected.as_deref() == Some(tags::AUTO);
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
            let left_score = self
                .quality
                .score(&left.tag, self.node_delays.get(&left.tag).copied());
            let right_score = self
                .quality
                .score(&right.tag, self.node_delays.get(&right.tag).copied());
            left_score
                .total_cmp(&right_score)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.tag.cmp(&right.tag))
        });
        ordered.truncate(MAX_LIST_ROWS);
        ordered.into_iter().map(|node| node.tag.clone()).collect()
    }

    pub(super) fn start_recovery(&mut self, trigger: String) {
        if self.recovery_in_flight
            || self.recovery_refresh_pending
            || self
                .health_pause_until
                .is_some_and(|at| at > Instant::now())
        {
            return;
        }
        if self.proxy.is_none() {
            return;
        }
        if !self.switch_budget_available() {
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
        let targets = self.probe_targets();
        let tx = self.events_tx.clone();
        let generation = self.health_generation;
        self.recovery_in_flight = true;
        self.notify(self.text().msg_health_recovering);
        self.spawn_guarded(BackgroundTask::Recovery, async move {
            let mut healthy = Vec::new();
            for chunk in candidates.chunks(DELAY_TEST_CONCURRENCY) {
                let mut tasks = JoinSet::new();
                for tag in chunk.iter().cloned() {
                    let client = client.clone();
                    let targets = targets.clone();
                    tasks.spawn(async move {
                        let delay = match probe_node_outcome(&client, &tag, &targets).await {
                            HealthOutcome::Healthy(delay) => Some(delay),
                            HealthOutcome::Failed | HealthOutcome::Unavailable => None,
                        };
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

    pub(super) fn set_switch_reason(&mut self, reason: &'static str) {
        self.pending_switch_reason = Some((reason, Instant::now()));
    }

    pub(super) fn record_auto_switch(&mut self) {
        self.auto_switch_times.push_back(Instant::now());
    }

    pub(super) fn switch_budget_available(&mut self) -> bool {
        let now = Instant::now();
        self.auto_switch_times
            .retain(|at| now.saturating_duration_since(*at) < AUTO_SWITCH_WINDOW);
        if self.auto_switch_times.len() < AUTO_SWITCH_BUDGET {
            return true;
        }
        self.auto_switch_times.clear();
        self.health_pause_until = Some(now + AUTO_SWITCH_PAUSE);
        self.logger.app(
            LogLevel::Warn,
            format!(
                "health failover paused: automatic switch budget used ({AUTO_SWITCH_BUDGET} in {}m)",
                AUTO_SWITCH_WINDOW.as_secs() / 60
            ),
        );
        self.notify(self.text().msg_health_budget_paused);
        false
    }

    fn selection_state(&self) -> Option<(bool, Option<String>)> {
        let selected = self.metrics.selection.selected.as_ref()?;
        if selected == tags::AUTO {
            Some((true, self.metrics.selection.auto_now.clone()))
        } else {
            Some((false, Some(selected.clone())))
        }
    }

    pub(super) fn observe_selection_change(&mut self, now: Instant) {
        let current = self.selection_state();
        if current == self.last_logged_selection {
            return;
        }
        if let (Some((auto, Some(tag))), Some((_, Some(previous)))) =
            (&current, &self.last_logged_selection)
        {
            let mode = if *auto { "auto" } else { "manual" };
            let reason = match self.pending_switch_reason.take() {
                Some((reason, at)) if now.saturating_duration_since(at) <= SWITCH_REASON_TTL => {
                    reason
                }
                _ if *auto => "urltest",
                _ => "manual",
            };
            self.logger.app(
                LogLevel::Info,
                format!("selection changed: {previous} -> {tag} (mode={mode}, reason={reason})"),
            );
        } else if let Some((auto, Some(tag))) = &current {
            let mode = if *auto { "auto" } else { "manual" };
            self.logger.app(
                LogLevel::Info,
                format!("selection active: {tag} (mode={mode})"),
            );
        }
        self.last_logged_selection = current;
    }

    pub(super) fn on_no_healthy_nodes(&mut self) {
        self.recovery_in_flight = false;
        self.reset_health_failures();
        self.schedule_no_node_pause();
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

    pub(super) fn finish_recovery_refresh(&mut self) {
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
}

/// Probe one node against multiple targets in priority order. A success on any
/// target means the node is healthy; only when every target reports a definite
/// node failure do we count the probe as failed. API/config errors remain
/// `Unavailable` so a broken Clash API cannot trigger a failover.
async fn probe_node_outcome(client: &ClashClient, tag: &str, targets: &[String]) -> HealthOutcome {
    let mut saw_failed = false;
    let mut saw_unavailable = false;
    for target in targets {
        match health_outcome(&client.delay(tag, target).await) {
            HealthOutcome::Healthy(delay) => return HealthOutcome::Healthy(delay),
            HealthOutcome::Failed => saw_failed = true,
            HealthOutcome::Unavailable => saw_unavailable = true,
        }
    }
    probe_failure_outcome(saw_failed, saw_unavailable)
}

pub(super) fn probe_failure_outcome(saw_failed: bool, saw_unavailable: bool) -> HealthOutcome {
    if saw_failed && !saw_unavailable {
        HealthOutcome::Failed
    } else {
        HealthOutcome::Unavailable
    }
}
