use super::*;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StartPlan {
    pub(crate) config: Config,
    pub(crate) nodes: Vec<Node>,
    pub(crate) preferred: Option<String>,
    pub(crate) binary_identity: Option<String>,
}

pub(crate) struct ProxyRuntime {
    pub(crate) status: ProxyStatus,
    pub(crate) proxy: Option<RunningProxy>,
    pub(crate) started_at: Option<Instant>,
    pub(crate) metrics_task: Option<JoinHandle<()>>,
    pub(crate) start_task: Option<JoinHandle<()>>,
    pub(crate) stop_task: Option<JoinHandle<()>>,
    pub(crate) restart_in_progress: bool,
    pub(crate) restart_after_stop: bool,
    pub(crate) preflight_in_flight: bool,
    pub(crate) preflight_revalidations: u32,
    pub(crate) preflight_force: bool,
    pub(crate) restart_attempts: u32,
    pub(crate) restart_at: Option<Instant>,
    pub(crate) stable_reset_at: Option<Instant>,
    pub(crate) runtime_has_auto: bool,
    pub(crate) proxy_node_tags: Vec<String>,
    pub(crate) proxy_auto_tags: HashSet<String>,
    pub(crate) proxy_node_names: HashMap<String, String>,
    pub(crate) last_running_plan: Option<StartPlan>,
    pub(crate) pending_restart_plan: Option<StartPlan>,
    pub(crate) restart_fallback_plan: Option<StartPlan>,
    pub(crate) epoch: u64,
}

impl Default for ProxyRuntime {
    fn default() -> Self {
        Self {
            status: ProxyStatus::Stopped,
            proxy: None,
            started_at: None,
            metrics_task: None,
            start_task: None,
            stop_task: None,
            restart_in_progress: false,
            restart_after_stop: false,
            preflight_in_flight: false,
            preflight_revalidations: 0,
            preflight_force: false,
            restart_attempts: 0,
            restart_at: None,
            stable_reset_at: None,
            runtime_has_auto: false,
            proxy_node_tags: Vec::new(),
            proxy_auto_tags: HashSet::new(),
            proxy_node_names: HashMap::new(),
            last_running_plan: None,
            pending_restart_plan: None,
            restart_fallback_plan: None,
            epoch: 0,
        }
    }
}

impl App {
    pub(super) fn perform_restart(&mut self) -> bool {
        if self.setup_busy() {
            self.notify(self.text().setup_busy);
            return false;
        }
        if self.proxy_busy() {
            self.notify(self.text().msg_proxy_busy);
            return false;
        }
        if self.proxy_runtime.status != ProxyStatus::Running || self.proxy_runtime.proxy.is_none() {
            self.notify(self.text().msg_proxy_not_running);
            return false;
        }
        self.begin_restart()
    }

    pub(super) fn auto_restart_after_change(&mut self) -> bool {
        if self.proxy_runtime.restart_in_progress {
            return false;
        }
        if self.proxy_runtime.status != ProxyStatus::Running || self.proxy_runtime.proxy.is_none() {
            return false;
        }
        if self
            .subscription_state
            .pending_restart_subscription
            .is_some()
        {
            return false;
        }
        if self.setup_busy() {
            return false;
        }
        if self.metrics_state.nodes.is_empty() {
            self.ui_state.last_error = Some(self.text().no_nodes.to_string());
            self.notify(self.text().no_nodes);
            return false;
        }
        let filtered = self.auto_candidate_tags();
        if filtered.is_empty() {
            self.ui_state.last_error = Some(self.text().no_matching_nodes.to_string());
            self.notify(self.text().no_matching_nodes);
            return false;
        }
        self.begin_restart()
    }

    pub(super) fn current_start_plan(&self) -> StartPlan {
        // Plan equality must not depend on the UI sort order, or display re-sorts
        // would keep invalidating restart preflights.
        let mut nodes = self.metrics_state.nodes.clone();
        nodes.sort_by(|left, right| left.tag.cmp(&right.tag));
        StartPlan {
            config: self.config.clone(),
            nodes,
            preferred: self.selection_state.preferred_node.clone(),
            binary_identity: self.binary_identity(),
        }
    }

    pub(super) fn binary_identity(&self) -> Option<String> {
        let path = self.binary_manager.locate()?;
        let file = std::fs::metadata(&path).ok()?;
        let modified = file
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let metadata = self.binary_manager.metadata();
        let version = metadata
            .as_ref()
            .map(|metadata| metadata.version.clone())
            .unwrap_or_else(|| "-".to_string());
        let digest = metadata
            .and_then(|metadata| metadata.sha256)
            .unwrap_or_default();
        Some(format!("{version}:{digest}:{}:{modified}", file.len()))
    }

    pub(super) fn begin_restart(&mut self) -> bool {
        if self.proxy_runtime.preflight_in_flight || self.proxy_runtime.restart_in_progress {
            return false;
        }
        self.ui_state.restart_confirm = false;
        self.ui_state.rollback_confirm = false;
        self.ui_state.subscription_delete = None;
        self.proxy_runtime.preflight_in_flight = true;
        self.proxy_runtime.restart_in_progress = true;
        self.proxy_runtime.preflight_force =
            self.proxy_runtime.preflight_revalidations >= MAX_PREFLIGHT_REVALIDATIONS;
        self.notify(self.text().msg_restarting);
        let paths = self.paths.clone();
        let plan = self.current_start_plan();
        let preflight_plan = plan.clone();
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::Preflight, async move {
            let result = supervisor::preflight(
                &paths,
                &preflight_plan.config,
                &preflight_plan.nodes,
                preflight_plan.preferred.as_deref(),
            )
            .await
            .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::PreflightDone {
                plan: Box::new(plan),
                result,
            });
        });
        true
    }

    pub(super) fn finish_restart_after_preflight(
        &mut self,
        plan: StartPlan,
        result: Result<(), String>,
    ) {
        self.proxy_runtime.preflight_in_flight = false;
        match result {
            Ok(()) => {
                if !self.proxy_runtime.preflight_force && plan != self.current_start_plan() {
                    self.proxy_runtime.restart_in_progress = false;
                    self.proxy_runtime.preflight_revalidations += 1;
                    self.logger.app(
                        LogLevel::Info,
                        "restart preflight invalidated by a state change; revalidating",
                    );
                    if self.proxy_runtime.proxy.is_some()
                        || self.proxy_runtime.status == ProxyStatus::Running
                    {
                        self.begin_restart();
                    } else {
                        let _ = self.start_proxy();
                    }
                    return;
                }
                if self.proxy_runtime.preflight_force {
                    self.logger.app(
                        LogLevel::Warn,
                        "restart preflight revalidation limit reached; applying the last preflighted plan",
                    );
                }
                self.proxy_runtime.preflight_revalidations = 0;
                self.proxy_runtime.preflight_force = false;
                if self.proxy_runtime.proxy.is_some() {
                    self.proxy_runtime.restart_after_stop = true;
                    self.proxy_runtime.pending_restart_plan = Some(plan);
                    self.stop_proxy();
                } else {
                    self.proxy_runtime.restart_in_progress = false;
                    let _ = self.start_proxy_with_plan(plan, None);
                }
            }
            Err(err) => {
                self.proxy_runtime.restart_in_progress = false;
                self.proxy_runtime.preflight_revalidations = 0;
                self.proxy_runtime.preflight_force = false;
                let message = format!("{}: {err}", self.text().msg_preflight_failed);
                self.ui_state.last_error = Some(message.clone());
                self.logger.app(LogLevel::Warn, message);
                self.notify(self.text().msg_preflight_failed);
                self.ui_state.restart_confirm = false;
                self.ui_state.rollback_confirm = false;
            }
        }
    }

    pub(super) fn start_proxy(&mut self) -> Result<(), String> {
        let plan = self.current_start_plan();
        self.start_proxy_with_plan(plan, None)
    }

    pub(super) fn start_proxy_with_plan(
        &mut self,
        plan: StartPlan,
        fallback: Option<StartPlan>,
    ) -> Result<(), String> {
        if self.proxy_busy() {
            return Err(self.text().msg_proxy_busy.to_string());
        }
        if self.binary_manager.locate().is_none() {
            self.open_setup_overlay();
            self.ui_state.last_error = Some(self.text().msg_singbox_missing.to_string());
            return Err(self.text().msg_singbox_missing.to_string());
        }
        if plan.nodes.is_empty() {
            self.ui_state.last_error = Some(self.text().no_nodes.to_string());
            self.switch_page(Page::Subscriptions);
            return Err(self.text().no_nodes.to_string());
        }
        let filtered = crate::selection::filter_node_tags(
            &plan.nodes,
            &plan.config.keywords_include,
            &plan.config.keywords_exclude,
        );
        if filtered.is_empty() {
            self.ui_state.last_error = Some(self.text().no_matching_nodes.to_string());
            return Err(self.text().no_matching_nodes.to_string());
        }
        self.proxy_runtime.runtime_has_auto = filtered.len() >= 2;
        self.proxy_runtime.proxy_auto_tags = filtered.into_iter().collect();
        let mut preferred = plan.preferred.clone();
        if let Some(tag) = preferred.as_deref() {
            if !plan.nodes.iter().any(|node| node.tag == tag) {
                preferred = None;
            }
        }
        self.proxy_runtime.proxy_node_tags =
            plan.nodes.iter().map(|node| node.tag.clone()).collect();
        self.proxy_runtime.proxy_node_names = plan
            .nodes
            .iter()
            .map(|node| (node.tag.clone(), node.name.clone()))
            .collect();
        self.proxy_runtime.status = ProxyStatus::Starting;
        self.proxy_runtime.epoch = self.proxy_runtime.epoch.wrapping_add(1);
        let epoch = self.proxy_runtime.epoch;
        self.proxy_runtime.restart_fallback_plan = fallback;
        let paths = self.paths.clone();
        let config = plan.config.clone();
        let nodes = plan.nodes.clone();
        let logger = self.logger.clone();
        let tx = self.events_tx.clone();
        let start_plan = plan.clone();
        let task = self.spawn_guarded_handle(BackgroundTask::ProxyStart, async move {
            let result = supervisor::start(&paths, &config, &nodes, &logger, preferred.as_deref())
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::ProxyStarted {
                epoch,
                plan: Box::new(start_plan),
                result,
            });
        });
        if let Some(previous) = self.proxy_runtime.start_task.replace(task) {
            previous.abort();
        }
        Ok(())
    }

    pub(super) fn schedule_restart(&mut self) {
        if self.proxy_runtime.restart_attempts >= MAX_RESTART_ATTEMPTS {
            self.proxy_runtime.restart_at = None;
            self.notify(self.text().msg_restart_paused);
            self.logger
                .app(LogLevel::Error, "restart paused after 5 failures");
            return;
        }
        let delay = RESTART_DELAYS[self.proxy_runtime.restart_attempts as usize];
        self.proxy_runtime.restart_attempts += 1;
        self.proxy_runtime.restart_at = Some(Instant::now() + Duration::from_secs(delay));
        self.notify(format!(
            "{}: {}s (#{})",
            self.text().msg_restart_scheduled,
            delay,
            self.proxy_runtime.restart_attempts
        ));
        self.logger.app(
            LogLevel::Warn,
            format!(
                "restart scheduled in {delay}s (attempt {}/{MAX_RESTART_ATTEMPTS})",
                self.proxy_runtime.restart_attempts
            ),
        );
    }

    pub(super) fn stop_proxy(&mut self) {
        if let Some(task) = self.proxy_runtime.metrics_task.take() {
            task.abort();
        }
        self.proxy_runtime.restart_at = None;
        self.proxy_runtime.restart_attempts = 0;
        self.proxy_runtime.stable_reset_at = None;
        self.reset_metrics();
        self.selection_state
            .selection_generation
            .fetch_add(1, Ordering::Relaxed);
        self.proxy_runtime.epoch = self.proxy_runtime.epoch.wrapping_add(1);
        self.proxy_runtime.preflight_revalidations = 0;
        self.proxy_runtime.preflight_force = false;
        let epoch = self.proxy_runtime.epoch;
        if let Some(proxy) = self.proxy_runtime.proxy.take() {
            self.proxy_runtime.status = ProxyStatus::Stopping;
            let logger = self.logger.clone();
            let tx = self.events_tx.clone();
            let task = self.spawn_guarded_handle(BackgroundTask::ProxyStop, async move {
                supervisor::stop(proxy, &logger).await;
                let _ = tx.send(AppEvent::ProxyStopped { epoch });
            });
            if let Some(previous) = self.proxy_runtime.stop_task.replace(task) {
                previous.abort();
            }
        } else {
            self.proxy_runtime.status = ProxyStatus::Stopped;
        }
    }
}
