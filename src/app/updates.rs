use super::*;
use crate::singbox::binary::is_newer_version;

// Only download when no binary is installed or the resolved release is strictly newer.
pub(super) fn download_needed(current: Option<&str>, latest: Option<&str>) -> bool {
    current.is_none_or(|current| latest.is_some_and(|latest| is_newer_version(latest, current)))
}

impl App {
    pub(super) fn refresh_previous_binary_state(&mut self) {
        let available = self.paths.previous_binary().is_file();
        self.binary_state.previous_binary_available = available;
        self.binary_state.previous_binary_version = if available {
            self.binary_manager
                .previous_metadata()
                .map(|metadata| metadata.version)
        } else {
            None
        };
    }

    pub(super) fn maybe_check_binary_update(&mut self) {
        if self.binary_state.update_check_in_flight || !self.binary_state.binary_installed {
            return;
        }
        if self
            .binary_state
            .last_update_check
            .is_some_and(|at| at.elapsed() < UPDATE_CHECK_INTERVAL)
        {
            return;
        }
        self.binary_state.update_check_in_flight = true;
        self.binary_state.last_update_check = Some(Instant::now());
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
        self.binary_state.setup.rollback_in_flight = true;
        self.binary_state.setup.message = None;
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
        if self.binary_state.setup.downloading || self.binary_state.install_in_flight {
            return;
        }
        if self.binary_state.setup.rollback_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        let current = if self.paths.binary().is_file() {
            self.binary_state.binary_version.clone()
        } else {
            None
        };
        let latest = self.binary_state.latest_binary_version.as_deref();
        if !download_needed(current.as_deref(), latest) {
            self.notify(format!(
                "{} {}",
                self.text().setup_current_version,
                current.as_deref().unwrap_or_default()
            ));
            return;
        }
        self.binary_state.setup.downloading = true;
        self.binary_state.setup.message = None;
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
        if self.binary_state.setup.downloading || self.binary_state.install_in_flight {
            return;
        }
        if self.binary_state.setup.rollback_in_flight {
            self.notify(self.text().setup_busy);
            return;
        }
        let Some(selected) = self.setup_overlay_selected() else {
            return;
        };
        let Some(candidate) = self.binary_state.setup.candidates.get(selected).cloned() else {
            return;
        };
        if !candidate.has_checksum
            && self.binary_state.setup.pending_install.as_ref() != Some(&candidate.path)
        {
            self.binary_state.setup.pending_install = Some(candidate.path.clone());
            self.binary_state.setup.message = None;
            return;
        }
        self.binary_state.setup.pending_install = None;
        self.binary_state.install_in_flight = true;
        self.binary_state.setup.installing = true;
        self.binary_state.setup.message = None;
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

#[cfg(test)]
mod tests {
    use super::download_needed;

    #[test]
    fn download_is_allowed_without_a_current_version_or_for_a_newer_release() {
        assert!(download_needed(None, None));
        assert!(download_needed(None, Some("1.14.4")));
        assert!(download_needed(Some("1.14.4"), Some("1.15.0")));
        assert!(download_needed(Some("1.15.0-alpha.1"), Some("1.15.0")));
    }

    #[test]
    fn download_is_blocked_for_the_same_or_an_older_release() {
        assert!(!download_needed(Some("1.15.0"), Some("1.15.0")));
        assert!(!download_needed(Some("1.15.0"), Some("1.14.4")));
        assert!(!download_needed(Some("1.15.0"), None));
    }
}
