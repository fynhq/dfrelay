//! sing-box binary update orchestration.
//!
//! Daily version checks, downloads, inbox installs, and previous-version
//! switching live here; the UI overlay and key handling stay in `app.rs`.

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

    pub(super) fn request_rollback(&mut self) {
        if self.setup_busy() {
            self.notify(self.text().setup_busy);
            return;
        }
        if !self.paths.previous_binary().is_file() {
            self.notify(self.text().msg_rollback_unavailable);
            return;
        }
        self.ui_state.rollback_confirm = true;
        self.ui_state.message = None;
    }

    pub(super) fn perform_rollback(&mut self) {
        if self.setup_busy() {
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
