use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::config::{Config, Paths};
use crate::fsutil;
use crate::ipc;
use crate::logging::{LogLevel, Logger};
use crate::service::{self, ServiceAction, ServiceIdentity, ServiceManager, ServiceMarkerState};
use crate::singbox::binary::{self, BinaryError};
use crate::version;

pub const GITHUB_API_LATEST: &str = "https://api.github.com/repos/fynhq/dfrelay/releases/latest";
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
pub const FAILED_COOLDOWN: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const MAX_DEFER: Duration = Duration::from_secs(12 * 60 * 60);

const MAX_SUMS_BYTES: usize = 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const START_TIMEOUT: Duration = Duration::from_secs(25);
const STABLE_WINDOW: Duration = Duration::from_secs(3);
const SERVICE_STOP_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("automatic updates are disabled for local builds")]
    LocalBuild,
    #[error("release lookup failed: {0}")]
    Api(String),
    #[error("no update asset for this platform")]
    NoAsset,
    #[error("release checksum is unavailable")]
    NoDigest,
    #[error("{0}")]
    Binary(String),
    #[error("archive error: {0}")]
    Archive(String),
    #[error("staged binary failed integrity verification")]
    StagedIntegrity,
    #[error("staged binary version mismatch: expected {expected}, got {actual}")]
    VersionMismatch { expected: String, actual: String },
    #[error("cannot write the update target: {0}")]
    Target(String),
    #[error("daemon did not stop or restart")]
    Restart,
    #[error("another update is already in progress")]
    Busy,
    #[error("automatic apply is not supported for this service manager: {0:?}")]
    Unsupported(ServiceManager),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: String,
    pub asset_name: String,
    pub asset_url: String,
    pub digest: Option<String>,
    pub sums_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateState {
    #[serde(default)]
    pub last_check_unix: Option<i64>,
    #[serde(default)]
    pub last_success_unix: Option<i64>,
    /// Newer release reported by the last successful check. The daemon also
    /// stages this version, but `dfr update check` only records it.
    #[serde(default)]
    pub available_version: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub failed_version: Option<String>,
    #[serde(default)]
    pub failed_at_unix: Option<i64>,
}

impl UpdateState {
    pub fn failed_recently(&self, version: &str) -> bool {
        self.failed_version.as_deref() == Some(version)
            && self.failed_at_unix.is_some_and(|at| {
                Local::now().timestamp().saturating_sub(at) < FAILED_COOLDOWN.as_secs() as i64
            })
    }

    /// A recorded release that is still newer than the running binary.
    pub fn available_update(&self) -> Option<&str> {
        self.available_version
            .as_deref()
            .filter(|version| binary::is_newer_version(version, version::BASE))
    }
}

pub fn is_release_build() -> bool {
    version::DISPLAY == version::BASE
}

pub fn asset_name(version: &str, os: &str, arch: &str) -> Option<String> {
    let name = match (os, arch) {
        ("linux", "x86_64") => format!("dfrelay_linux_x64_v{version}_musl.tar.gz"),
        ("linux", "aarch64" | "arm64") => format!("dfrelay_linux_arm64_v{version}_musl.tar.gz"),
        ("android", "aarch64" | "arm64") => format!("dfrelay_android_arm64_v{version}.tar.gz"),
        ("macos", "x86_64") => format!("dfrelay_macos_x64_v{version}.tar.gz"),
        ("macos", "aarch64" | "arm64") => format!("dfrelay_macos_arm64_v{version}.tar.gz"),
        _ => return None,
    };
    Some(name)
}

pub fn state_path(paths: &Paths) -> PathBuf {
    paths.updates_dir.join("state.json")
}

pub fn discover_staged(paths: &Paths) -> Option<(String, PathBuf)> {
    let mut best: Option<(String, PathBuf, std::time::SystemTime)> = None;
    for entry in fs::read_dir(&paths.updates_dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(version) = name.to_str().and_then(|name| name.strip_prefix("dfr-")) else {
            continue;
        };
        // The integrity sidecar is not a staged binary.
        if version.ends_with(".sha256") {
            continue;
        }
        if !binary::is_newer_version(version, version::BASE) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(_, _, stamp)| modified > *stamp) {
            best = Some((version.to_string(), path, modified));
        }
    }
    best.map(|(version, path, _)| (version, path))
}

pub fn load_state(paths: &Paths) -> UpdateState {
    fs::read_to_string(state_path(paths))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_state(paths: &Paths, state: &UpdateState) -> std::io::Result<()> {
    fsutil::ensure_private_dir(&paths.updates_dir)?;
    let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    fsutil::write_private_atomic(&state_path(paths), &bytes)
}

/// Record a successful check. `available` is the newer release, if any; a
/// check that found nothing clears a previously recorded version.
pub fn record_check_result(paths: &Paths, available: Option<&str>) -> std::io::Result<()> {
    let mut state = load_state(paths);
    state.last_check_unix = Some(Local::now().timestamp());
    state.available_version = available.map(str::to_string);
    save_state(paths, &state)
}

pub async fn check_for_update(config: &Config) -> Result<Option<ReleaseInfo>, UpdateError> {
    let latest = fetch_latest(config).await?;
    if binary::is_newer_version(&latest.version, version::BASE) {
        Ok(Some(latest))
    } else {
        Ok(None)
    }
}

async fn fetch_latest(config: &Config) -> Result<ReleaseInfo, UpdateError> {
    let clients = binary::download_clients(config).map_err(binary_error)?;
    let mut last_error = String::from("release API unavailable");
    for channel in &clients {
        match fetch_latest_with(&channel.client).await {
            Ok(release) => return Ok(release),
            Err(err) => last_error = format!("{}: {err}", channel.channel.label()),
        }
    }
    Err(UpdateError::Api(last_error))
}

async fn fetch_latest_with(client: &reqwest::Client) -> Result<ReleaseInfo, UpdateError> {
    let response = client
        .get(GITHUB_API_LATEST)
        .send()
        .await
        .map_err(|err| UpdateError::Api(err.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(UpdateError::Api(format!("HTTP {}", status.as_u16())));
    }
    crate::http_body::ensure_https(&response).map_err(UpdateError::Api)?;
    // Read with a streaming cap: a chunked response has no Content-Length and
    // must not be buffered without bound.
    let bytes = crate::http_body::read_limited(response, crate::http_body::JSON_BODY_LIMIT)
        .await
        .map_err(UpdateError::Api)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| UpdateError::Api(err.to_string()))?;
    let tag = value
        .get("tag_name")
        .and_then(|item| item.as_str())
        .unwrap_or_default();
    let version = tag.trim_start_matches('v').to_string();
    if version.is_empty() || version.contains('-') {
        return Err(UpdateError::Api(
            "latest release is not a stable version".into(),
        ));
    }
    if value.get("prerelease").and_then(|item| item.as_bool()) == Some(true) {
        return Err(UpdateError::Api("latest release is a prerelease".into()));
    }
    let asset_name = asset_name(&version, std::env::consts::OS, std::env::consts::ARCH)
        .ok_or(UpdateError::NoAsset)?;
    let assets = value
        .get("assets")
        .and_then(|item| item.as_array())
        .ok_or_else(|| UpdateError::Api("release has no assets".into()))?;
    let asset = assets
        .iter()
        .find(|item| item.get("name").and_then(|name| name.as_str()) == Some(asset_name.as_str()))
        .ok_or(UpdateError::NoAsset)?;
    let asset_url = asset
        .get("browser_download_url")
        .and_then(|item| item.as_str())
        .ok_or(UpdateError::NoAsset)?
        .to_string();
    if !asset_url.starts_with("https://") {
        return Err(UpdateError::Api("release asset URL is not HTTPS".into()));
    }
    let digest = asset
        .get("digest")
        .and_then(|item| item.as_str())
        .map(str::to_string);
    let sums_url = assets
        .iter()
        .find(|item| item.get("name").and_then(|name| name.as_str()) == Some("SHA256SUMS"))
        .and_then(|item| item.get("browser_download_url"))
        .and_then(|item| item.as_str())
        .map(str::to_string);
    Ok(ReleaseInfo {
        version,
        asset_name,
        asset_url,
        digest,
        sums_url,
    })
}

pub async fn download_and_stage(
    config: &Config,
    paths: &Paths,
    release: &ReleaseInfo,
    logger: &Logger,
) -> Result<PathBuf, UpdateError> {
    let clients = binary::download_clients(config).map_err(binary_error)?;
    let digest = match release.digest.clone() {
        Some(digest) => digest,
        None => fetch_sums_digest(&clients, release).await?,
    };
    let attempts = binary::download_attempts(&clients, &config.github_mirrors, &release.asset_url);
    for (kind, client, url) in attempts {
        logger.app(
            LogLevel::Info,
            format!("app update download attempt {kind}: {url}"),
        );
        match binary::download_and_verify(client, &url, &digest).await {
            Ok(bytes) => {
                let binary_bytes = extract_dfrt(&bytes)?;
                let staged = stage_binary(paths, &release.version, &binary_bytes)?;
                if let Err(err) = verify_binary_version(&staged, &release.version).await {
                    let _ = fs::remove_file(&staged);
                    let _ = fs::remove_file(staged_sidecar(&paths.updates_dir, &release.version));
                    return Err(err);
                }
                logger.app(
                    LogLevel::Info,
                    format!("app update {} staged from {kind}", release.version),
                );
                return Ok(staged);
            }
            Err(err) => logger.app(
                LogLevel::Warn,
                format!("app update download attempt {kind} failed: {err}"),
            ),
        }
    }
    Err(UpdateError::Api("all download attempts failed".into()))
}

async fn fetch_sums_digest(
    clients: &[binary::ChannelClient],
    release: &ReleaseInfo,
) -> Result<String, UpdateError> {
    let url = release.sums_url.as_deref().ok_or(UpdateError::NoDigest)?;
    if !url.starts_with("https://") {
        return Err(UpdateError::NoDigest);
    }
    for channel in clients {
        let mut response = match channel.client.get(url).send().await {
            Ok(response) if response.status().is_success() => response,
            _ => continue,
        };
        if crate::http_body::ensure_https(&response).is_err() {
            continue;
        }
        if response
            .content_length()
            .is_some_and(|length| length as usize > MAX_SUMS_BYTES)
        {
            continue;
        }
        let mut bytes = Vec::new();
        let mut failed = false;
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if bytes.len().saturating_add(chunk.len()) > MAX_SUMS_BYTES {
                        failed = true;
                        break;
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        if failed {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        if let Some(digest) = parse_sha256sums(&text, &release.asset_name) {
            return Ok(digest);
        }
    }
    Err(UpdateError::NoDigest)
}

pub fn parse_sha256sums(text: &str, asset: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(digest) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next().map(|name| name.trim_start_matches('*')) else {
            continue;
        };
        if name == asset
            && digest.len() == 64
            && digest.chars().all(|item| item.is_ascii_hexdigit())
        {
            return Some(digest.to_string());
        }
    }
    None
}

fn extract_dfrt(bytes: &[u8]) -> Result<Vec<u8>, UpdateError> {
    crate::archive::extract_tar_gz_entry(
        bytes,
        "dfr",
        MAX_BINARY_BYTES,
        "archive entry is too large",
    )
    .map_err(UpdateError::Archive)
}

fn staged_sidecar(dir: &Path, version: &str) -> PathBuf {
    dir.join(format!("dfr-{version}.sha256"))
}

fn stage_binary(paths: &Paths, version: &str, bytes: &[u8]) -> Result<PathBuf, UpdateError> {
    fsutil::ensure_private_dir(&paths.updates_dir)?;
    let staged = paths.updates_dir.join(format!("dfr-{version}"));
    let temp = paths.updates_dir.join(format!(".dfr-{version}.tmp"));
    let sidecar = staged_sidecar(&paths.updates_dir, version);
    let mut renamed = false;
    let result = (|| -> Result<(), UpdateError> {
        fs::write(&temp, bytes)?;
        fsutil::set_executable(&temp)?;
        fs::File::open(&temp)?.sync_all()?;
        fs::rename(&temp, &staged)?;
        renamed = true;
        // Record the digest of the exact staged bytes so a later `apply_staged`
        // (possibly after a daemon restart) can detect replacement or corruption.
        fsutil::write_private_file(&sidecar, binary::sha256_hex(bytes).as_bytes())?;
        Ok(())
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(&temp);
        if renamed {
            let _ = fs::remove_file(&staged);
            let _ = fs::remove_file(&sidecar);
        }
        return Err(err);
    }
    // A newer staged version supersedes older downloads and their sidecars;
    // interrupted `.tmp` files are also useless once this stage succeeded.
    cleanup_stale_staged(paths, Some(version));
    Ok(staged)
}

/// Remove staged binaries, sidecars and interrupted temp files that are not the
/// current candidate. `state.json` and `apply.lock` are control files and are
/// always preserved.
pub(crate) fn cleanup_stale_staged(paths: &Paths, keep_version: Option<&str>) {
    let Ok(entries) = fs::read_dir(&paths.updates_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == "state.json" || name == "apply.lock" {
            continue;
        }
        if name.starts_with(".dfr-") && name.ends_with(".tmp") {
            let _ = fs::remove_file(entry.path());
            continue;
        }
        let Some(rest) = name.strip_prefix("dfr-") else {
            continue;
        };
        let version = rest.strip_suffix(".sha256").unwrap_or(rest);
        if keep_version == Some(version) {
            continue;
        }
        let _ = fs::remove_file(entry.path());
    }
}

fn verify_staged_integrity(paths: &Paths, staged: &Path, version: &str) -> Result<(), UpdateError> {
    let sidecar = staged_sidecar(&paths.updates_dir, version);
    let expected = fs::read_to_string(&sidecar)
        .map_err(|_| UpdateError::StagedIntegrity)?
        .trim()
        .to_string();
    if expected.len() != 64 {
        return Err(UpdateError::StagedIntegrity);
    }
    let bytes = fs::read(staged).map_err(|_| UpdateError::StagedIntegrity)?;
    if binary::sha256_hex(&bytes) != expected {
        return Err(UpdateError::StagedIntegrity);
    }
    Ok(())
}

pub async fn version_of_binary(path: &Path) -> Result<String, UpdateError> {
    let output = tokio::time::timeout(
        VERSION_TIMEOUT,
        tokio::process::Command::new(path)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| UpdateError::Binary("version check timed out".into()))?
    .map_err(|err| UpdateError::Binary(err.to_string()))?;
    if !output.status.success() {
        return Err(UpdateError::Binary(format!(
            "`{} --version` exited with {}",
            path.display(),
            output.status
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_dfr_version(&text)
        .ok_or_else(|| UpdateError::Binary("binary did not report a recognizable version".into()))
}

/// Parse the clap-style `dfr <version>` line strictly. Taking the last
/// whitespace token would accept diagnostic text appended by a wrapper or a
/// corrupted binary and compare it as a version.
fn parse_dfr_version(output: &str) -> Option<String> {
    let line = output.lines().next()?.trim();
    let mut parts = line.split_whitespace();
    if parts.next()? != "dfr" {
        return None;
    }
    let value = parts.next()?.trim_start_matches('v');
    if parts.next().is_some() || value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

pub async fn verify_binary_version(path: &Path, expected: &str) -> Result<(), UpdateError> {
    let result = version_of_binary(path).await?;
    if result != expected {
        return Err(UpdateError::VersionMismatch {
            expected: expected.to_string(),
            actual: result,
        });
    }
    Ok(())
}

pub fn target_binary() -> Result<PathBuf, UpdateError> {
    if !is_release_build() {
        return Err(UpdateError::LocalBuild);
    }
    // The version suffix (absent only for release builds) is the reliable
    // signal; do not guess from path components, which false-positive for
    // installations such as `/opt/target/bin/dfr`.
    Ok(fs::canonicalize(std::env::current_exe()?).unwrap_or(std::env::current_exe()?))
}

pub fn previous_binary(target: &Path) -> Result<PathBuf, UpdateError> {
    let parent = target
        .parent()
        .ok_or_else(|| UpdateError::Target("target has no parent directory".into()))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| UpdateError::Target("target has no file name".into()))?;
    Ok(parent.join(format!("{name}.previous")))
}

pub fn install_staged(staged: &Path, target: &Path) -> Result<PathBuf, UpdateError> {
    let previous = previous_binary(target)?;
    let parent = target
        .parent()
        .ok_or_else(|| UpdateError::Target("target has no parent directory".into()))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| UpdateError::Target("target has no file name".into()))?;
    if target.exists() {
        fs::copy(target, &previous).map_err(|err| UpdateError::Target(err.to_string()))?;
    }
    let temp = parent.join(format!(".{name}.update.tmp"));
    let result = (|| -> Result<(), UpdateError> {
        fs::copy(staged, &temp).map_err(|err| UpdateError::Target(err.to_string()))?;
        fsutil::set_executable(&temp)?;
        fs::File::open(&temp)?.sync_all()?;
        fs::rename(&temp, target).map_err(|err| UpdateError::Target(err.to_string()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(previous)
}

pub fn restore_previous(target: &Path, previous: &Path) -> Result<(), UpdateError> {
    if !previous.is_file() {
        return Err(UpdateError::Target("previous binary is missing".into()));
    }
    let parent = target
        .parent()
        .ok_or_else(|| UpdateError::Target("target has no parent directory".into()))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| UpdateError::Target("target has no file name".into()))?;
    let failed = parent.join(format!("{name}.failed"));
    let _ = fs::copy(target, &failed);
    let temp = parent.join(format!(".{name}.rollback.tmp"));
    let result = (|| -> Result<(), UpdateError> {
        fs::copy(previous, &temp).map_err(|err| UpdateError::Target(err.to_string()))?;
        fsutil::set_executable(&temp)?;
        fs::File::open(&temp)?.sync_all()?;
        fs::rename(&temp, target).map_err(|err| UpdateError::Target(err.to_string()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn check_target_writable(target: &Path) -> Result<(), UpdateError> {
    let parent = target
        .parent()
        .ok_or_else(|| UpdateError::Target("target has no parent directory".into()))?;
    let probe = parent.join(format!(".dfr.write-test.{}", std::process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(err) => Err(UpdateError::Target(err.to_string())),
    }
}

pub fn auto_apply_allowed(
    has_viewer: bool,
    active_connections: bool,
    pending_for: Duration,
) -> bool {
    !has_viewer && (!active_connections || pending_for >= MAX_DEFER)
}

pub struct UpdateLock {
    #[cfg(unix)]
    file: std::fs::File,
    #[cfg(not(unix))]
    path: PathBuf,
}

impl UpdateLock {
    pub fn acquire(paths: &Paths) -> Result<Self, UpdateError> {
        fsutil::ensure_private_dir(&paths.updates_dir)?;
        let path = paths.updates_dir.join("apply.lock");
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let file = fsutil::open_private_file(&path, false)?;
            // An advisory lock is released automatically when the file handle
            // closes, including on process exit, so there is no stale-lock
            // guesswork or drop-time race.
            let locked =
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
            if !locked {
                return Err(UpdateError::Busy);
            }
            Ok(Self { file })
        }
        #[cfg(not(unix))]
        {
            use std::io::Write;

            for attempt in 0..2 {
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(mut file) => {
                        let _ = write!(file, "{}", std::process::id());
                        return Ok(Self { path });
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                        let stale = fs::metadata(&path)
                            .and_then(|metadata| metadata.modified())
                            .ok()
                            .and_then(|modified| modified.elapsed().ok())
                            .is_some_and(|age| age > Duration::from_secs(3600));
                        if !stale {
                            return Err(UpdateError::Busy);
                        }
                        let _ = fs::remove_file(&path);
                    }
                    Err(err) => return Err(UpdateError::Io(err)),
                }
            }
            Err(UpdateError::Busy)
        }
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
            // Keep the lock file: removing it can race with another process
            // that has just opened the same path and is about to lock it.
        }
        #[cfg(not(unix))]
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub async fn apply_staged(
    paths: &Paths,
    staged: &Path,
    target: &Path,
    expected_version: &str,
    logger: &Logger,
) -> Result<(), UpdateError> {
    let _lock = UpdateLock::acquire(paths)?;
    check_target_writable(target)?;
    // Re-verify the staged bytes before swapping the binary: the file was
    // written and verified in an earlier process and may have been replaced.
    verify_staged_integrity(paths, staged, expected_version)?;
    let socket = paths.control_socket();
    let identity = stop_daemon(paths, &socket).await?;
    wait_socket_gone(&socket, SERVICE_STOP_TIMEOUT).await?;
    let previous = match install_staged(staged, target) {
        Ok(previous) => previous,
        Err(err) => {
            let _ = start_daemon(identity.as_ref(), target, paths).await;
            return Err(err);
        }
    };
    let started = start_daemon(identity.as_ref(), target, paths).await;
    let status = match started {
        Ok(()) => wait_for_stable_daemon(&socket, START_TIMEOUT, Some(expected_version)).await,
        Err(err) => {
            logger.app(
                LogLevel::Warn,
                format!("app update: cannot start new daemon: {err}"),
            );
            None
        }
    };
    let Some(status) = status else {
        return rollback(
            paths,
            target,
            &previous,
            identity.as_ref(),
            logger,
            expected_version,
            UpdateError::Restart,
        )
        .await;
    };
    if status.version != expected_version {
        let err = UpdateError::VersionMismatch {
            expected: expected_version.to_string(),
            actual: status.version,
        };
        logger.app(
            LogLevel::Warn,
            format!("app update: running daemon reported a different version: {err}"),
        );
        return rollback(
            paths,
            target,
            &previous,
            identity.as_ref(),
            logger,
            expected_version,
            err,
        )
        .await;
    }
    if let Err(err) = verify_binary_version(target, expected_version).await {
        logger.app(
            LogLevel::Warn,
            format!("app update: new binary verification failed: {err}"),
        );
        return rollback(
            paths,
            target,
            &previous,
            identity.as_ref(),
            logger,
            expected_version,
            err,
        )
        .await;
    }
    let mut state = load_state(paths);
    state.last_success_unix = Some(Local::now().timestamp());
    state.last_error = None;
    state.failed_version = None;
    state.failed_at_unix = None;
    let _ = save_state(paths, &state);
    let _ = fs::remove_file(staged);
    let _ = fs::remove_file(staged_sidecar(&paths.updates_dir, expected_version));
    cleanup_stale_staged(paths, None);
    logger.app(
        LogLevel::Info,
        format!("app update: {expected_version} installed and running"),
    );
    Ok(())
}

pub async fn rollback_to_previous(
    paths: &Paths,
    target: &Path,
    logger: &Logger,
) -> Result<(), UpdateError> {
    let _lock = UpdateLock::acquire(paths)?;
    let previous = previous_binary(target)?;
    if !previous.is_file() {
        return Err(UpdateError::Target(
            "no previous binary is available".into(),
        ));
    }
    check_target_writable(target)?;
    let previous_version = version_of_binary(&previous).await.ok();
    let socket = paths.control_socket();
    let identity = stop_daemon(paths, &socket).await?;
    wait_socket_gone(&socket, SERVICE_STOP_TIMEOUT).await?;
    if let Err(err) = restore_previous(target, &previous) {
        let _ = start_daemon(identity.as_ref(), target, paths).await;
        return Err(err);
    }
    start_daemon(identity.as_ref(), target, paths).await?;
    if wait_for_stable_daemon(&socket, START_TIMEOUT, previous_version.as_deref())
        .await
        .is_some()
    {
        logger.app(
            LogLevel::Info,
            "app update: rolled back to the previous binary",
        );
        Ok(())
    } else {
        Err(UpdateError::Restart)
    }
}

async fn rollback(
    paths: &Paths,
    target: &Path,
    previous: &Path,
    identity: Option<&ServiceIdentity>,
    logger: &Logger,
    failed_version: &str,
    cause: UpdateError,
) -> Result<(), UpdateError> {
    logger.app(
        LogLevel::Warn,
        format!("app update: rolling back {failed_version}: {cause}"),
    );
    let failure = cause.to_string();
    let previous_version = version_of_binary(previous).await.ok();
    let socket = paths.control_socket();
    // Stop the instance that was started for verification before touching the
    // binary. If the stop fails, leave both the disk state and the running
    // process alone, record the failure, and let the user decide.
    if let Err(err) = stop_daemon(paths, &socket).await {
        record_update_failure(
            paths,
            failed_version,
            &format!("cannot stop the new daemon: {err}; original failure: {failure}"),
        );
        return Err(err);
    }
    if let Err(err) = wait_socket_gone(&socket, SERVICE_STOP_TIMEOUT).await {
        record_update_failure(
            paths,
            failed_version,
            &format!("control socket did not close: {err}; original failure: {failure}"),
        );
        return Err(err);
    }
    if let Err(err) = restore_previous(target, previous) {
        record_update_failure(
            paths,
            failed_version,
            &format!("cannot restore the previous binary: {err}; original failure: {failure}"),
        );
        return Err(err);
    }
    // Persist the failure before starting the restored daemon. The new process
    // reads `state.json` during App::new; writing afterwards lets it load the
    // same staged update again and retry a version that just failed.
    record_update_failure(paths, failed_version, &failure);
    if let Err(err) = start_daemon(identity, target, paths).await {
        record_update_failure(
            paths,
            failed_version,
            &format!(
                "restored binary but cannot start the daemon: {err}; original failure: {failure}"
            ),
        );
        return Err(err);
    }
    let restored = wait_for_stable_daemon(&socket, START_TIMEOUT, previous_version.as_deref())
        .await
        .is_some();
    if restored {
        logger.app(
            LogLevel::Warn,
            format!("app update: restored the previous binary after {failed_version} failed"),
        );
    } else {
        logger.app(
            LogLevel::Error,
            "app update: rollback completed but the daemon did not start",
        );
    }
    Err(UpdateError::Restart)
}

pub(crate) fn record_update_failure(paths: &Paths, failed_version: &str, message: &str) {
    let mut state = load_state(paths);
    state.failed_version = Some(failed_version.to_string());
    state.failed_at_unix = Some(Local::now().timestamp());
    state.last_error = Some(message.to_string());
    let _ = save_state(paths, &state);
}

/// Record a helper-side apply failure for the daemon's cooldown check. `Busy`
/// means another updater holds the lock; it is transient and must not suppress
/// the version for the full cooldown.
pub(crate) fn record_helper_failure(paths: &Paths, version: &str, err: &UpdateError) {
    if !matches!(err, UpdateError::Busy) {
        record_update_failure(paths, version, &err.to_string());
    }
}

async fn stop_daemon(paths: &Paths, socket: &Path) -> Result<Option<ServiceIdentity>, UpdateError> {
    let running = ipc::daemon_is_running(socket).await;
    let status = if running {
        ipc::control_status(socket).await.ok()
    } else {
        None
    };
    if status.as_ref().is_some_and(|status| status.managed)
        && status
            .as_ref()
            .and_then(|status| status.service.clone())
            .is_none()
    {
        return Err(UpdateError::Restart);
    }
    let marker = service::load_marker(&paths.data_dir);
    let identity = status
        .as_ref()
        .and_then(|status| status.service.clone())
        .or_else(|| match &marker {
            Ok(Some(ServiceMarkerState::Managed(identity))) => Some(identity.clone()),
            _ => None,
        });
    if identity.is_none() && matches!(marker, Ok(Some(ServiceMarkerState::ManagedWithoutIdentity)))
    {
        return Err(UpdateError::Restart);
    }
    if let Some(identity) = &identity {
        let identity = identity.clone();
        tokio::task::spawn_blocking(move || service::control(&identity, ServiceAction::Stop, None))
            .await
            .map_err(|_| UpdateError::Restart)?
            .map_err(|_| UpdateError::Restart)?;
    } else if running {
        ipc::control_shutdown(socket)
            .await
            .map_err(|_| UpdateError::Restart)?;
    }
    Ok(identity)
}

async fn start_daemon(
    identity: Option<&ServiceIdentity>,
    target: &Path,
    paths: &Paths,
) -> Result<(), UpdateError> {
    if let Some(identity) = identity {
        let identity = identity.clone();
        tokio::task::spawn_blocking(move || {
            service::control(&identity, ServiceAction::Start, None)
        })
        .await
        .map_err(|_| UpdateError::Restart)?
        .map_err(|_| UpdateError::Restart)?;
    } else {
        ipc::spawn_daemon_at(
            target,
            Some(paths.config_file.clone()),
            Some(paths.data_dir.clone()),
        )?;
    }
    Ok(())
}

async fn wait_socket_gone(socket: &Path, timeout: Duration) -> Result<(), UpdateError> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if !ipc::daemon_is_running(socket).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(UpdateError::Restart)
}

fn stable_window_ok(first: &crate::view::DaemonStatus, second: &crate::view::DaemonStatus) -> bool {
    // Uptime is reported in whole seconds; allow one second of rounding.
    let required_uptime = first
        .uptime_secs
        .saturating_add(STABLE_WINDOW.as_secs().saturating_sub(1));
    second.pid == first.pid
        && second.version == first.version
        && second.uptime_secs >= required_uptime
}

async fn wait_for_stable_daemon(
    socket: &Path,
    timeout: Duration,
    expected_version: Option<&str>,
) -> Option<crate::view::DaemonStatus> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let Ok(first) = ipc::control_status(socket).await else {
            tokio::time::sleep(Duration::from_millis(150)).await;
            continue;
        };
        if expected_version.is_some_and(|expected| first.version != expected) {
            return None;
        }
        tokio::time::sleep(STABLE_WINDOW).await;
        let Ok(second) = ipc::control_status(socket).await else {
            return None;
        };
        return stable_window_ok(&first, &second).then_some(second);
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
enum HelperLaunch {
    Direct,
    Command {
        program: PathBuf,
        args: Vec<OsString>,
    },
}

fn helper_args(paths: &Paths, staged: &Path, target: &Path, version: &str) -> Vec<OsString> {
    vec![
        OsString::from("update"),
        OsString::from("helper"),
        OsString::from("--staged"),
        staged.as_os_str().to_os_string(),
        OsString::from("--target"),
        target.as_os_str().to_os_string(),
        OsString::from("--version"),
        OsString::from(version),
        OsString::from("--config"),
        paths.config_file.as_os_str().to_os_string(),
        OsString::from("--data"),
        paths.data_dir.as_os_str().to_os_string(),
    ]
}

fn helper_launch_plan(
    identity: Option<&ServiceIdentity>,
    executable: &Path,
    args: &[OsString],
    suffix: &str,
) -> Result<HelperLaunch, UpdateError> {
    let Some(identity) = identity else {
        return Ok(HelperLaunch::Direct);
    };
    match identity.manager {
        ServiceManager::SystemdUser => {
            let mut command_args: Vec<OsString> = vec![
                OsString::from("--user"),
                OsString::from(format!("--unit=dfrelay-update-{suffix}")),
                OsString::from("--collect"),
                OsString::from("--no-block"),
                OsString::from("--"),
                executable.as_os_str().to_os_string(),
            ];
            command_args.extend(args.iter().cloned());
            Ok(HelperLaunch::Command {
                program: systemd_run_path(),
                args: command_args,
            })
        }
        ServiceManager::LaunchdGui => {
            let label = format!("com.dfrelay.update.{suffix}");
            // `launchctl submit` keeps a failed job alive, which would turn a
            // broken staged update into a restart loop and leave the job label
            // behind. Run the helper through a shell that always removes the
            // job and exits successfully, so launchd never relaunches it.
            let mut command_args: Vec<OsString> = vec![
                OsString::from("submit"),
                OsString::from("-l"),
                OsString::from(label.clone()),
                OsString::from("--"),
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(
                    "\"$@\" || true; /bin/launchctl remove \"$0\" >/dev/null 2>&1 || true; exit 0",
                ),
                OsString::from(label),
                executable.as_os_str().to_os_string(),
            ];
            command_args.extend(args.iter().cloned());
            Ok(HelperLaunch::Command {
                program: PathBuf::from("/bin/launchctl"),
                args: command_args,
            })
        }
        manager => Err(UpdateError::Unsupported(manager)),
    }
}

fn systemd_run_path() -> PathBuf {
    for path in ["/usr/bin/systemd-run", "/bin/systemd-run"] {
        let path = Path::new(path);
        if path.is_file() {
            return path.to_path_buf();
        }
    }
    PathBuf::from("systemd-run")
}

pub fn spawn_apply_helper(
    paths: &Paths,
    staged: &Path,
    target: &Path,
    version: &str,
    identity: Option<&ServiceIdentity>,
) -> Result<(), UpdateError> {
    let executable = std::env::current_exe()?;
    let args = helper_args(paths, staged, target, version);
    let suffix = format!("{}-{}", std::process::id(), Local::now().timestamp());
    match helper_launch_plan(identity, &executable, &args, &suffix)? {
        HelperLaunch::Direct => {
            let mut command = Command::new(&executable);
            command
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    command.pre_exec(|| {
                        libc::setsid();
                        Ok(())
                    });
                }
            }
            command.spawn()?;
        }
        HelperLaunch::Command { program, args } => {
            Command::new(program)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
        }
    }
    Ok(())
}

fn binary_error(err: BinaryError) -> UpdateError {
    UpdateError::Binary(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn daemon_status(pid: u32, version: &str, uptime_secs: u64) -> crate::view::DaemonStatus {
        crate::view::DaemonStatus {
            pid,
            managed: false,
            service: None,
            proxy: crate::state::ProxyStatus::Stopped,
            uptime_secs,
            listen: "127.0.0.1:12000".into(),
            current_node: None,
            version: version.into(),
        }
    }

    #[test]
    fn stable_window_requires_the_same_process_and_version() {
        let first = daemon_status(7, "1.2.3", 0);
        assert!(stable_window_ok(
            &first,
            &daemon_status(7, "1.2.3", STABLE_WINDOW.as_secs())
        ));
        assert!(!stable_window_ok(&first, &daemon_status(8, "1.2.3", 3)));
        assert!(!stable_window_ok(&first, &daemon_status(7, "1.2.4", 3)));
        assert!(!stable_window_ok(&first, &daemon_status(7, "1.2.3", 1)));
    }

    #[test]
    fn helper_failure_records_deterministic_errors_but_not_busy() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        record_helper_failure(&paths, "9.9.9", &UpdateError::Busy);
        assert!(!load_state(&paths).failed_recently("9.9.9"));
        record_helper_failure(&paths, "9.9.9", &UpdateError::StagedIntegrity);
        assert!(load_state(&paths).failed_recently("9.9.9"));
    }

    #[test]
    fn failure_state_records_version_and_message() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        record_update_failure(&paths, "9.9.9", "boom");
        let state = load_state(&paths);
        assert_eq!(state.failed_version.as_deref(), Some("9.9.9"));
        assert_eq!(state.last_error.as_deref(), Some("boom"));
        assert!(state.failed_recently("9.9.9"));
    }

    #[test]
    fn parses_only_the_exact_version_line() {
        assert_eq!(parse_dfr_version("dfr 1.2.3\n"), Some("1.2.3".into()));
        assert_eq!(parse_dfr_version("dfr v1.2.3\n"), Some("1.2.3".into()));
        assert_eq!(
            parse_dfr_version("dfr 1.2.3+abc\n"),
            Some("1.2.3+abc".into())
        );
        assert_eq!(parse_dfr_version("dfr 1.2.3 extra\n"), None);
        assert_eq!(parse_dfr_version("garbage 1.2.3\n"), None);
        assert_eq!(parse_dfr_version(""), None);
    }

    #[test]
    fn asset_names_match_release_artifacts() {
        assert_eq!(
            asset_name("1.2.3", "linux", "x86_64").as_deref(),
            Some("dfrelay_linux_x64_v1.2.3_musl.tar.gz")
        );
        assert_eq!(
            asset_name("1.2.3", "linux", "aarch64").as_deref(),
            Some("dfrelay_linux_arm64_v1.2.3_musl.tar.gz")
        );
        assert_eq!(
            asset_name("1.2.3", "android", "aarch64").as_deref(),
            Some("dfrelay_android_arm64_v1.2.3.tar.gz")
        );
        assert_eq!(
            asset_name("1.2.3", "macos", "x86_64").as_deref(),
            Some("dfrelay_macos_x64_v1.2.3.tar.gz")
        );
        assert_eq!(
            asset_name("1.2.3", "macos", "aarch64").as_deref(),
            Some("dfrelay_macos_arm64_v1.2.3.tar.gz")
        );
        assert!(asset_name("1.2.3", "windows", "x86_64").is_none());
    }

    #[test]
    fn sha256sums_parser_finds_the_matching_asset() {
        let digest = "a".repeat(64);
        let text = format!(
            "\n# comment\nmalformed\n{}  other.tar.gz\n{} *dfrelay_linux_x64_v1.2.3_musl.tar.gz\n",
            "b".repeat(64),
            digest
        );
        assert_eq!(
            parse_sha256sums(&text, "dfrelay_linux_x64_v1.2.3_musl.tar.gz").as_deref(),
            Some(digest.as_str())
        );
        assert!(parse_sha256sums(&text, "missing.tar.gz").is_none());
        assert!(parse_sha256sums("notahash  target.tar.gz", "target.tar.gz").is_none());
    }

    #[test]
    fn failed_version_is_ignored_during_cooldown() {
        let mut state = UpdateState {
            failed_version: Some("1.2.3".into()),
            failed_at_unix: Some(Local::now().timestamp()),
            ..Default::default()
        };
        assert!(state.failed_recently("1.2.3"));
        assert!(!state.failed_recently("1.2.4"));
        state.failed_at_unix =
            Some(Local::now().timestamp() - FAILED_COOLDOWN.as_secs() as i64 - 1);
        assert!(!state.failed_recently("1.2.3"));
    }

    #[test]
    fn auto_apply_waits_for_viewers_and_long_connections() {
        assert!(auto_apply_allowed(false, false, Duration::ZERO));
        assert!(!auto_apply_allowed(
            true,
            false,
            MAX_DEFER + Duration::from_secs(1)
        ));
        assert!(!auto_apply_allowed(false, true, Duration::ZERO));
        assert!(auto_apply_allowed(false, true, MAX_DEFER));
    }

    #[test]
    fn install_and_restore_keep_the_previous_binary() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("dfr");
        let staged = dir.path().join("staged");
        fs::write(&target, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();
        let previous = install_staged(&staged, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read(&previous).unwrap(), b"old");
        restore_previous(&target, &previous).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"old");
    }

    #[test]
    fn state_roundtrips_through_disk() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        let state = UpdateState {
            last_check_unix: Some(123),
            available_version: Some("9.9.9".into()),
            failed_version: Some("1.2.3".into()),
            ..Default::default()
        };
        save_state(&paths, &state).unwrap();
        let loaded = load_state(&paths);
        assert_eq!(loaded.last_check_unix, Some(123));
        assert_eq!(loaded.available_version.as_deref(), Some("9.9.9"));
        assert_eq!(loaded.failed_version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn check_result_records_and_clears_the_available_version() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        record_check_result(&paths, Some("9.9.9")).unwrap();
        let state = load_state(&paths);
        assert!(state.last_check_unix.is_some());
        assert_eq!(state.available_update(), Some("9.9.9"));
        record_check_result(&paths, None).unwrap();
        let state = load_state(&paths);
        assert!(state.available_version.is_none());
        assert!(state.available_update().is_none());
    }

    #[test]
    fn extract_dfrt_reads_the_expected_archive_entry() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut archive = tar::Builder::new(&mut encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, "package/dfr", &b"bin"[..])
                .unwrap();
            archive.finish().unwrap();
        }
        let bytes = encoder.finish().unwrap();
        assert_eq!(extract_dfrt(&bytes).unwrap(), b"bin");
    }

    fn service_identity(manager: ServiceManager) -> ServiceIdentity {
        ServiceIdentity {
            manager,
            unit: "dfrelay.service".to_string(),
            target: None,
            uid: 1000,
        }
    }

    #[test]
    fn helper_launch_plan_uses_the_service_manager() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        let args = helper_args(
            &paths,
            Path::new("/tmp/staged"),
            Path::new("/tmp/target"),
            "1.2.3",
        );
        let executable = Path::new("/usr/local/bin/dfr");

        assert_eq!(
            helper_launch_plan(None, executable, &args, "1-2").unwrap(),
            HelperLaunch::Direct
        );

        let plan = helper_launch_plan(
            Some(&service_identity(ServiceManager::SystemdUser)),
            executable,
            &args,
            "1-2",
        )
        .unwrap();
        let HelperLaunch::Command { program, args } = plan else {
            panic!("expected a command plan");
        };
        assert_eq!(
            program.file_name().and_then(|name| name.to_str()),
            Some("systemd-run")
        );
        for expected in [
            "--user",
            "--collect",
            "--no-block",
            "--unit=dfrelay-update-1-2",
            "/usr/local/bin/dfr",
            "--staged",
        ] {
            assert!(
                args.iter().any(|arg| arg.to_string_lossy() == expected),
                "missing {expected}"
            );
        }

        let plan = helper_launch_plan(
            Some(&service_identity(ServiceManager::LaunchdGui)),
            executable,
            &args,
            "1-2",
        )
        .unwrap();
        let HelperLaunch::Command { program, args } = plan else {
            panic!("expected a command plan");
        };
        assert_eq!(program, PathBuf::from("/bin/launchctl"));
        assert_eq!(args[0], OsString::from("submit"));
        assert_eq!(args[1], OsString::from("-l"));
        assert_eq!(args[2], OsString::from("com.dfrelay.update.1-2"));
        assert_eq!(args[3], OsString::from("--"));
        assert_eq!(args[4], OsString::from("/bin/sh"));
        assert_eq!(args[5], OsString::from("-c"));
        assert!(args[6].to_string_lossy().contains("launchctl remove"));
        assert_eq!(args[7], OsString::from("com.dfrelay.update.1-2"));
        assert_eq!(args[8], OsString::from("/usr/local/bin/dfr"));

        assert!(matches!(
            helper_launch_plan(
                Some(&service_identity(ServiceManager::SystemdSystem)),
                executable,
                &args,
                "1-2"
            ),
            Err(UpdateError::Unsupported(ServiceManager::SystemdSystem))
        ));
    }

    #[test]
    fn cleanup_removes_old_staged_files_and_preserves_control_files() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        paths.ensure().unwrap();
        for name in [
            "dfr-0.1.0",
            "dfr-0.1.0.sha256",
            "dfr-9.9.9",
            "dfr-9.9.9.sha256",
            ".dfr-9.9.9.tmp",
            "state.json",
            "apply.lock",
        ] {
            fs::write(paths.updates_dir.join(name), b"x").unwrap();
        }
        cleanup_stale_staged(&paths, Some("9.9.9"));
        assert!(!paths.updates_dir.join("dfr-0.1.0").exists());
        assert!(!paths.updates_dir.join("dfr-0.1.0.sha256").exists());
        assert!(paths.updates_dir.join("dfr-9.9.9").exists());
        assert!(paths.updates_dir.join("dfr-9.9.9.sha256").exists());
        assert!(!paths.updates_dir.join(".dfr-9.9.9.tmp").exists());
        assert!(paths.updates_dir.join("state.json").exists());
        assert!(paths.updates_dir.join("apply.lock").exists());
    }

    #[test]
    fn discover_staged_ignores_old_versions_and_picks_a_candidate() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        paths.ensure().unwrap();
        fs::write(paths.updates_dir.join("dfr-0.1.0"), b"old").unwrap();
        fs::write(paths.updates_dir.join("dfr-99.0.0"), b"new").unwrap();
        let (version, path) = discover_staged(&paths).expect("staged update");
        assert_eq!(version, "99.0.0");
        assert_eq!(path, paths.updates_dir.join("dfr-99.0.0"));
    }

    #[test]
    fn update_lock_rejects_a_concurrent_apply() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        let lock = UpdateLock::acquire(&paths).unwrap();
        assert!(matches!(
            UpdateLock::acquire(&paths),
            Err(UpdateError::Busy)
        ));
        drop(lock);
        assert!(UpdateLock::acquire(&paths).is_ok());
    }

    #[test]
    fn staged_integrity_rejects_tampered_bytes() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        let staged = stage_binary(&paths, "9.9.9", b"original").unwrap();
        assert!(verify_staged_integrity(&paths, &staged, "9.9.9").is_ok());

        // A sidecar must not be mistaken for a staged binary.
        assert!(discover_staged(&paths).is_some());

        fs::write(&staged, b"tampered").unwrap();
        assert!(matches!(
            verify_staged_integrity(&paths, &staged, "9.9.9"),
            Err(UpdateError::StagedIntegrity)
        ));

        fs::remove_file(&staged).unwrap();
        assert!(matches!(
            verify_staged_integrity(&paths, &staged, "9.9.9"),
            Err(UpdateError::StagedIntegrity)
        ));
    }
}
