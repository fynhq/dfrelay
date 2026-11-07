//! Health probing and failover for the active node.
//!
//! Method group extracted from `app.rs`; `App` itself and its fields stay in
//! the parent module, so this file only adds an `impl App` block.

use super::*;

impl App {
    pub(super) fn health_target(&self) -> Option<String> {
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
        self.spawn_guarded(BackgroundTask::HealthProbe, async move {
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

    pub(super) fn reset_health_failures(&mut self) {
        self.health_failures = 0;
        self.health_failure_tag = None;
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
    }

    pub(super) fn recovery_candidates(&self, trigger: &str) -> Vec<String> {
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

    pub(super) fn start_recovery(&mut self, trigger: String) {
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
        self.spawn_guarded(BackgroundTask::Recovery, async move {
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
