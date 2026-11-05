use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Local;
use flate2::read::GzDecoder;
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
const LOCK_STALE: Duration = Duration::from_secs(60 * 60);
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
    if response.url().scheme() != "https" {
        return Err(UpdateError::Api(
            "release API redirected to a non-HTTPS URL".into(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > 4 * 1024 * 1024)
    {
        return Err(UpdateError::Api("release response is too large".into()));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|err| UpdateError::Api(err.to_string()))?;
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
    let mut attempts: Vec<(&str, &reqwest::Client, String)> = clients
        .iter()
        .map(|channel| {
            (
                channel.channel.label(),
                &channel.client,
                release.asset_url.clone(),
            )
        })
        .collect();
    for prefix in &config.github_mirrors {
        attempts.push((
            "mirror",
            &clients[0].client,
            binary::mirror_url(prefix, &release.asset_url),
        ));
    }
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
        if response.url().scheme() != "https" {
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
        let digest = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
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
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut found: Option<Vec<u8>> = None;
    for entry in archive
        .entries()
        .map_err(|err| UpdateError::Archive(err.to_string()))?
    {
        let entry = entry.map_err(|err| UpdateError::Archive(err.to_string()))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|err| UpdateError::Archive(err.to_string()))?;
        if path.file_name().and_then(|name| name.to_str()) != Some("dfr") {
            continue;
        }
        if path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            return Err(UpdateError::Archive("unsafe archive path".into()));
        }
        if entry.size() > MAX_BINARY_BYTES {
            return Err(UpdateError::Archive("archive entry is too large".into()));
        }
        if found.is_some() {
            return Err(UpdateError::Archive(
                "archive contains multiple dfr files".into(),
            ));
        }
        let mut binary = Vec::new();
        entry
            .take(MAX_BINARY_BYTES + 1)
            .read_to_end(&mut binary)
            .map_err(|err| UpdateError::Archive(err.to_string()))?;
        if binary.len() as u64 > MAX_BINARY_BYTES {
            return Err(UpdateError::Archive("archive entry is too large".into()));
        }
        found = Some(binary);
    }
    found.ok_or_else(|| UpdateError::Archive("dfr not found in archive".into()))
}

fn stage_binary(paths: &Paths, version: &str, bytes: &[u8]) -> Result<PathBuf, UpdateError> {
    fsutil::ensure_private_dir(&paths.updates_dir)?;
    let staged = paths.updates_dir.join(format!("dfr-{version}"));
    let temp = paths.updates_dir.join(format!(".dfr-{version}.tmp"));
    fs::write(&temp, bytes)?;
    set_executable(&temp)?;
    fs::File::open(&temp)?.sync_all()?;
    fs::rename(&temp, &staged)?;
    Ok(staged)
}

pub fn version_of_binary(path: &Path) -> Result<String, UpdateError> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|err| UpdateError::Binary(err.to_string()))?;
    if !output.status.success() {
        return Err(UpdateError::Binary(format!(
            "`{} --version` exited with {}",
            path.display(),
            output.status
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let value = text
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .trim_start_matches('v');
    if value.is_empty() {
        return Err(UpdateError::Binary(
            "binary did not report a version".into(),
        ));
    }
    Ok(value.to_string())
}

pub async fn verify_binary_version(path: &Path, expected: &str) -> Result<(), UpdateError> {
    let path = path.to_path_buf();
    let expected = expected.to_string();
    let result = tokio::time::timeout(
        VERSION_TIMEOUT,
        tokio::task::spawn_blocking(move || version_of_binary(&path)),
    )
    .await
    .map_err(|_| UpdateError::Binary("version check timed out".into()))?
    .map_err(|err| UpdateError::Binary(err.to_string()))??;
    if result != expected {
        return Err(UpdateError::VersionMismatch {
            expected,
            actual: result,
        });
    }
    Ok(())
}

pub fn target_binary() -> Result<PathBuf, UpdateError> {
    if !is_release_build() {
        return Err(UpdateError::LocalBuild);
    }
    let executable = std::env::current_exe()?;
    let executable = fs::canonicalize(&executable).unwrap_or(executable);
    if executable
        .components()
        .any(|component| component.as_os_str() == "target")
    {
        return Err(UpdateError::LocalBuild);
    }
    Ok(executable)
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
        set_executable(&temp)?;
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
        set_executable(&temp)?;
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

fn set_executable(path: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

pub fn auto_apply_allowed(
    has_viewer: bool,
    active_connections: bool,
    pending_for: Duration,
) -> bool {
    !has_viewer && (!active_connections || pending_for >= MAX_DEFER)
}

pub struct UpdateLock {
    path: PathBuf,
}

impl UpdateLock {
    pub fn acquire(paths: &Paths) -> Result<Self, UpdateError> {
        fsutil::ensure_private_dir(&paths.updates_dir)?;
        let path = paths.updates_dir.join("apply.lock");
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
                        .is_some_and(|age| age > LOCK_STALE);
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

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
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
        Ok(()) => wait_for_stable_daemon(&socket, START_TIMEOUT).await,
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
    let socket = paths.control_socket();
    let identity = stop_daemon(paths, &socket).await?;
    wait_socket_gone(&socket, SERVICE_STOP_TIMEOUT).await?;
    if let Err(err) = restore_previous(target, &previous) {
        let _ = start_daemon(identity.as_ref(), target, paths).await;
        return Err(err);
    }
    start_daemon(identity.as_ref(), target, paths).await?;
    if wait_for_stable_daemon(&socket, START_TIMEOUT)
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
    restore_previous(target, previous)?;
    start_daemon(identity, target, paths).await?;
    let socket = paths.control_socket();
    let restored = wait_for_stable_daemon(&socket, START_TIMEOUT)
        .await
        .is_some();
    let mut state = load_state(paths);
    state.failed_version = Some(failed_version.to_string());
    state.failed_at_unix = Some(Local::now().timestamp());
    state.last_error = Some(cause.to_string());
    let _ = save_state(paths, &state);
    if restored {
        logger.app(
            LogLevel::Warn,
            format!("app update: restored the previous binary after {failed_version} failed"),
        );
        Err(UpdateError::Restart)
    } else {
        logger.app(
            LogLevel::Error,
            "app update: rollback completed but the daemon did not start",
        );
        Err(UpdateError::Restart)
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
        service::control(identity, ServiceAction::Stop, None).map_err(|_| UpdateError::Restart)?;
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
        service::control(identity, ServiceAction::Start, None).map_err(|_| UpdateError::Restart)?;
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

async fn wait_for_stable_daemon(
    socket: &Path,
    timeout: Duration,
) -> Option<crate::view::DaemonStatus> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if ipc::control_status(socket).await.is_ok() {
            tokio::time::sleep(STABLE_WINDOW).await;
            return ipc::control_status(socket).await.ok();
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
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
            let mut command_args: Vec<OsString> = vec![
                OsString::from("submit"),
                OsString::from("-l"),
                OsString::from(format!("com.dfrelay.update.{suffix}")),
                OsString::from("--"),
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
            "{}  other.tar.gz\n{} *dfrelay_linux_x64_v1.2.3_musl.tar.gz\n",
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
            failed_version: Some("1.2.3".into()),
            ..Default::default()
        };
        save_state(&paths, &state).unwrap();
        let loaded = load_state(&paths);
        assert_eq!(loaded.last_check_unix, Some(123));
        assert_eq!(loaded.failed_version.as_deref(), Some("1.2.3"));
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
        assert_eq!(args[4], OsString::from("/usr/local/bin/dfr"));

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
}
