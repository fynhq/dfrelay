//! Binary and application update orchestration.
//!
//! Daily checks, staged app-update application, and the update-policy cycle
//! live here; the UI overlay and key handling stay in `app.rs`.

use super::*;

impl App {
    pub(super) fn refresh_previous_binary_state(&mut self) {
        let available = self.paths.previous_binary().is_file();
        self.previous_binary_available = available;
        self.previous_binary_version = if available {
            self.binary_manager
                .previous_metadata()
                .map(|metadata| metadata.version)
        } else {
            None
        };
    }

    pub(super) fn maybe_check_binary_update(&mut self) {
        if self.update_check_in_flight || !self.binary_installed {
            return;
        }
        if self
            .last_update_check
            .is_some_and(|at| at.elapsed() < UPDATE_CHECK_INTERVAL)
        {
            return;
        }
        self.update_check_in_flight = true;
        self.last_update_check = Some(Instant::now());
        let manager = self.binary_manager.clone();
        let config = self.config.clone();
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::BinaryUpdateCheck, async move {
            let result = manager
                .latest_version(&config)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::UpdateCheckDone(result));
        });
    }

    pub(super) fn maybe_check_app_update(&mut self) {
        if self.config.app_update == UpdatePolicy::Off
            || self.app_update_in_flight
            || self.app_update_downloading
            || self.pending_app_update.is_some()
            || !update::is_release_build()
        {
            return;
        }
        if self
            .last_app_update_check
            .is_some_and(|at| at.elapsed() < update::CHECK_INTERVAL)
        {
            return;
        }
        if self
            .app_update_retry_at
            .is_some_and(|at| at > Instant::now())
        {
            return;
        }
        self.app_update_in_flight = true;
        self.last_app_update_check = Some(Instant::now());
        let config = self.config.clone();
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::AppUpdateCheck, async move {
            let result = update::check_for_update(&config)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::AppUpdateChecked(result));
        });
    }

    pub(super) fn maybe_apply_app_update(&mut self) {
        if self.config.app_update != UpdatePolicy::Auto || !update::is_release_build() {
            return;
        }
        if self.app_update_in_flight {
            if self
                .app_update_apply_started
                .is_some_and(|at| at.elapsed() > Duration::from_secs(600))
            {
                self.app_update_in_flight = false;
                self.app_update_apply_started = None;
                self.logger.app(
                    LogLevel::Warn,
                    "app update: applier did not restart the daemon; retrying",
                );
            } else {
                return;
            }
        }
        let Some(pending) = self.pending_app_update.as_ref() else {
            return;
        };
        if !update::auto_apply_allowed(
            self.has_viewer(),
            !self.metrics.connections.is_empty(),
            pending.since.elapsed(),
        ) {
            if !self.app_update_deferred_logged {
                self.logger.app(
                    LogLevel::Info,
                    "app update: deferred until the TUI detaches or activity settles",
                );
                self.app_update_deferred_logged = true;
            }
            return;
        }
        if self.managed && self.service.is_none() {
            if !self.app_update_deferred_logged {
                self.logger.app(
                    LogLevel::Info,
                    "app update: service-managed daemon without identity; run `dfr update apply`",
                );
                self.app_update_deferred_logged = true;
            }
            return;
        }
        let target = match update::target_binary() {
            Ok(target) => target,
            Err(err) => {
                self.logger.app(
                    LogLevel::Warn,
                    format!("app update: cannot resolve the installed binary: {err}"),
                );
                self.app_update_deferred_logged = true;
                return;
            }
        };
        let staged = pending.staged.clone();
        let version = pending.version.clone();
        match update::spawn_apply_helper(
            &self.paths,
            &staged,
            &target,
            &version,
            self.service.as_ref(),
        ) {
            Ok(()) => {
                self.app_update_in_flight = true;
                self.app_update_apply_started = Some(Instant::now());
                self.logger.app(
                    LogLevel::Info,
                    format!("app update: applying v{version}; daemon will restart"),
                );
                self.notify(self.text().msg_app_update_applying);
            }
            Err(err) => {
                self.logger.app(
                    LogLevel::Warn,
                    format!("app update: cannot start the applier: {err}"),
                );
                self.app_update_deferred_logged = true;
            }
        }
    }

    pub(super) fn cycle_app_update_policy(&mut self) {
        let mut config = self.config.clone();
        config.app_update = config.app_update.next();
        if let Err(err) = config.save(&self.paths.config_file) {
            self.last_error = Some(err.to_string());
            return;
        }
        self.config = config;
        self.last_app_update_check = None;
        self.app_update_retry_at = None;
        self.app_update_deferred_logged = false;
        self.notify(self.text().settings_saved);
        self.logger.app(
            LogLevel::Info,
            format!("app update policy: {}", self.config.app_update.as_str()),
        );
    }

    pub(super) fn request_rollback(&mut self) {
        if self.setup.downloading || self.setup.rollback_in_flight || self.install_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        if !self.paths.previous_binary().is_file() {
            self.notify(self.text().msg_rollback_unavailable);
            return;
        }
        self.rollback_confirm = true;
        self.message = None;
    }

    pub(super) fn perform_rollback(&mut self) {
        if self.setup.downloading || self.setup.rollback_in_flight || self.install_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        if !self.paths.previous_binary().is_file() {
            self.notify(self.text().msg_rollback_unavailable);
            return;
        }
        self.setup.rollback_in_flight = true;
        self.setup.message = None;
        let manager = self.binary_manager.clone();
        let logger = self.logger.clone();
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::BinaryRollback, async move {
            let result = manager
                .rollback(&logger)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::RollbackDone(result));
        });
    }

    pub(super) fn start_download(&mut self) {
        if self.setup.downloading || self.install_in_flight {
            return;
        }
        if self.setup.rollback_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        self.setup.downloading = true;
        self.setup.message = None;
        let config = self.config.clone();
        let logger = self.logger.clone();
        let manager = self.binary_manager.clone();
        let tx = self.events_tx.clone();
        self.spawn_guarded(BackgroundTask::BinaryDownload, async move {
            let result = manager
                .download_latest(&config, &logger)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::DownloadDone(result));
        });
    }

    pub(super) async fn install_selected(&mut self) {
        if self.setup.downloading || self.install_in_flight {
            return;
        }
        if self.setup.rollback_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        let Some(selected) = self.setup_overlay_selected() else {
            return;
        };
        let Some(candidate) = self.setup.candidates.get(selected).cloned() else {
            return;
        };
        if !candidate.has_checksum && self.setup.pending_install.as_ref() != Some(&candidate.path) {
            self.setup.pending_install = Some(candidate.path.clone());
            self.setup.message = None;
            return;
        }
        self.setup.pending_install = None;
        self.install_in_flight = true;
        self.setup.installing = true;
        self.setup.message = None;
        let manager = self.binary_manager.clone();
        let logger = self.logger.clone();
        let tx = self.events_tx.clone();
        let path = candidate.path.clone();
        self.spawn_guarded(BackgroundTask::BinaryInstall, async move {
            let result = manager
                .install_from_inbox(&path, &logger)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.send(AppEvent::InstallDone(result));
        });
    }
}
