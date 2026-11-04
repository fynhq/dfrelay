use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Local;
use flate2::read::GzDecoder;
use reqwest::{Client, Proxy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Config, Paths};
use crate::fsutil;
use crate::logging::{LogLevel, Logger};

const GITHUB_API_LATEST: &str = "https://api.github.com/repos/SagerNet/sing-box/releases/latest";
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ASSET_BYTES: usize = 100 * 1024 * 1024;
const MAX_MANUAL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryMetadata {
    pub version: String,
    pub source: String,
    pub sha256: Option<String>,
    pub installed_at: String,
    pub verified: bool,
}

#[derive(Debug, Clone)]
pub struct ReleaseAsset {
    pub version: String,
    pub name: String,
    pub url: String,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InboxCandidate {
    pub path: PathBuf,
    pub label: String,
    pub has_checksum: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BinaryError {
    #[error("network: {0}")]
    Network(String),
    #[error("HTTP {0}")]
    Http(u16),
    #[error("release API unavailable or no digest; manual download required")]
    ManualRequired,
    #[error("no matching release asset for this platform")]
    NoAsset,
    #[error("SHA-256 mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("archive error: {0}")]
    Archive(String),
    #[error("binary version mismatch: expected {expected}, got {actual}")]
    VersionMismatch { expected: String, actual: String },
    #[error("cannot read version from binary: {0}")]
    Version(String),
    #[error("no previous sing-box binary is available")]
    NoPrevious,
    #[error("sing-box check failed: {0}")]
    Check(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct BinaryManager {
    paths: Paths,
}

impl BinaryManager {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    pub fn locate(&self) -> Option<PathBuf> {
        let managed = self.paths.binary();
        managed.is_file().then_some(managed)
    }

    pub fn metadata(&self) -> Option<BinaryMetadata> {
        let text = fs::read_to_string(self.paths.binary_metadata()).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn previous_metadata(&self) -> Option<BinaryMetadata> {
        let text = fs::read_to_string(self.paths.previous_binary_metadata()).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub async fn version(&self, binary: &Path) -> Result<String, BinaryError> {
        let output = tokio::time::timeout(
            VERSION_TIMEOUT,
            tokio::process::Command::new(binary)
                .arg("version")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| BinaryError::Version("timed out".to_string()))?
        .map_err(BinaryError::Io)?;
        if !output.status.success() {
            return Err(BinaryError::Version(format!(
                "exit status {}",
                output.status
            )));
        }
        parse_version_output(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(|| BinaryError::Version("unexpected output".to_string()))
    }

    pub async fn latest_version(&self, config: &Config) -> Result<String, BinaryError> {
        let clients = download_clients(config)?;
        let asset = resolve_latest_release(&clients, GITHUB_API_LATEST, None).await?;
        Ok(asset.version)
    }

    pub async fn download_latest(
        &self,
        config: &Config,
        logger: &Logger,
    ) -> Result<BinaryMetadata, BinaryError> {
        logger.app(LogLevel::Info, "resolving latest sing-box release");
        let clients = download_clients(config)?;
        let asset = resolve_latest_release(&clients, GITHUB_API_LATEST, Some(logger))
            .await
            .map_err(|err| {
                logger.app(
                    LogLevel::Warn,
                    format!("GitHub release API unavailable: {err}"),
                );
                BinaryError::ManualRequired
            })?;
        let digest = asset.digest.clone().ok_or(BinaryError::ManualRequired)?;

        let mut attempts = Vec::new();
        for channel in &clients {
            attempts.push((channel.channel.label(), &channel.client, asset.url.clone()));
        }
        for prefix in &config.github_mirrors {
            attempts.push(("mirror", &clients[0].client, mirror_url(prefix, &asset.url)));
        }

        for (kind, client, url) in attempts {
            logger.app(LogLevel::Info, format!("download attempt {kind}: {url}"));
            match download_and_verify(client, &url, &digest).await {
                Ok(bytes) => match self.install_archive(&bytes, &asset, &digest, true).await {
                    Ok(metadata) => {
                        logger.app(
                            LogLevel::Info,
                            format!("installed sing-box {} from {kind}", metadata.version),
                        );
                        return Ok(metadata);
                    }
                    Err(err) => {
                        logger.app(
                            LogLevel::Warn,
                            format!("install attempt {kind} failed: {err}"),
                        );
                    }
                },
                Err(err) => {
                    logger.app(
                        LogLevel::Warn,
                        format!("download attempt {kind} failed: {err}"),
                    );
                }
            }
        }

        logger.app(
            LogLevel::Warn,
            "all automatic download attempts failed; manual inbox required",
        );
        Err(BinaryError::ManualRequired)
    }

    pub fn scan_inbox(&self) -> Result<Vec<InboxCandidate>, BinaryError> {
        fsutil::ensure_private_dir(&self.paths.inbox_dir)?;
        let mut candidates = Vec::new();
        for entry in fs::read_dir(&self.paths.inbox_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".sha256") {
                continue;
            }
            if !is_supported_manual_file(&name) {
                continue;
            }
            let checksum = PathBuf::from(format!("{}.sha256", path.display()));
            candidates.push(InboxCandidate {
                label: name,
                has_checksum: checksum.is_file(),
                path,
            });
        }
        candidates.sort_by(|a, b| a.label.cmp(&b.label));
        Ok(candidates)
    }

    pub async fn install_from_inbox(
        &self,
        path: &Path,
        logger: &Logger,
    ) -> Result<BinaryMetadata, BinaryError> {
        if fs::metadata(path)?.len() > MAX_MANUAL_BYTES {
            return Err(BinaryError::Archive("manual file is too large".to_string()));
        }
        let bytes = fs::read(path)?;
        let checksum = read_sidecar_checksum(path)?;
        if let Some(expected) = checksum.as_deref() {
            let actual = sha256_hex(&bytes);
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(BinaryError::DigestMismatch {
                    expected: expected.to_string(),
                    actual,
                });
            }
        }
        let binary = extract_binary_bytes(path, &bytes)?;
        let temp = self.write_temp_binary(&binary)?;
        let actual_version = match self.version(&temp).await {
            Ok(version) => version,
            Err(err) => {
                let _ = fs::remove_file(&temp);
                return Err(err);
            }
        };
        if let Err(err) = self.check_existing_config(&temp).await {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        let verified = checksum.is_some();
        self.install_temp_binary(&temp, &actual_version, path, checksum.as_deref(), verified)?;
        logger.app(
            LogLevel::Info,
            format!(
                "installed sing-box {actual_version} from inbox{}",
                if checksum.is_some() {
                    " (verified)"
                } else {
                    " (unverified)"
                }
            ),
        );
        Ok(BinaryMetadata {
            version: actual_version,
            source: path.display().to_string(),
            sha256: checksum,
            installed_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            verified,
        })
    }

    pub async fn rollback(&self, logger: &Logger) -> Result<BinaryMetadata, BinaryError> {
        let previous = self.paths.previous_binary();
        if !previous.is_file() {
            return Err(BinaryError::NoPrevious);
        }
        let version = self.version(&previous).await?;
        self.check_binary(&previous).await?;

        let target = self.paths.binary();
        let swap = self.paths.bin_dir.join("sing-box.rollback");
        let current_metadata = fs::read_to_string(self.paths.binary_metadata()).ok();
        let previous_metadata = self.previous_metadata();
        let had_current = target.exists();

        let _ = fs::remove_file(&swap);
        if had_current {
            fs::rename(&target, &swap)?;
        }
        if let Err(err) = fs::rename(&previous, &target) {
            if had_current {
                let _ = fs::rename(&swap, &target);
            }
            return Err(err.into());
        }
        if had_current {
            if let Err(err) = fs::rename(&swap, &previous) {
                let _ = fs::rename(&target, &previous);
                let _ = fs::rename(&swap, &target);
                return Err(err.into());
            }
            if let Some(text) = current_metadata.as_deref() {
                fsutil::write_private_atomic(
                    &self.paths.previous_binary_metadata(),
                    text.as_bytes(),
                )?;
            } else {
                let _ = fs::remove_file(self.paths.previous_binary_metadata());
            }
        } else {
            let _ = fs::remove_file(self.paths.previous_binary_metadata());
        }

        let metadata = previous_metadata
            .filter(|metadata| metadata.version == version)
            .unwrap_or_else(|| self.minimal_metadata(&version));
        self.write_metadata(&metadata)?;
        logger.app(LogLevel::Info, format!("rolled back sing-box to {version}"));
        Ok(metadata)
    }

    async fn install_archive(
        &self,
        bytes: &[u8],
        asset: &ReleaseAsset,
        digest: &str,
        verified: bool,
    ) -> Result<BinaryMetadata, BinaryError> {
        let actual = sha256_hex(bytes);
        let expected = digest.trim_start_matches("sha256:");
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(BinaryError::DigestMismatch {
                expected: expected.to_string(),
                actual,
            });
        }
        let binary = extract_binary_bytes(Path::new(&asset.name), bytes)?;
        let temp = self.write_temp_binary(&binary)?;
        let version = match self.version(&temp).await {
            Ok(version) => version,
            Err(err) => {
                let _ = fs::remove_file(&temp);
                return Err(err);
            }
        };
        if let Err(err) = self.check_existing_config(&temp).await {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        if version != asset.version {
            let _ = fs::remove_file(&temp);
            return Err(BinaryError::VersionMismatch {
                expected: asset.version.clone(),
                actual: version,
            });
        }
        self.install_temp_binary(
            &temp,
            &version,
            Path::new(&asset.url),
            Some(digest),
            verified,
        )?;
        Ok(BinaryMetadata {
            version,
            source: asset.url.clone(),
            sha256: Some(digest.to_string()),
            installed_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            verified,
        })
    }

    /// Check a new binary against the currently generated runtime configuration.
    ///
    /// The minimal config only proves that the executable can start. Upgrade
    /// compatibility must also cover the node fields in the live configuration;
    /// otherwise a new sing-box can replace the old one and fail at the next
    /// proxy start. Fall back to the minimal check when no runtime config exists.
    async fn check_existing_config(&self, binary: &Path) -> Result<(), BinaryError> {
        let runtime = self.paths.runtime_config();
        if runtime.is_file() {
            self.check_config_file(binary, &runtime).await
        } else {
            self.check_binary(binary).await
        }
    }

    async fn check_binary(&self, binary: &Path) -> Result<(), BinaryError> {
        let config = self
            .paths
            .bin_dir
            .join(format!(".check-{}.json", std::process::id()));
        fsutil::write_private_file(
            &config,
            br#"{"log":{"level":"info"},"outbounds":[{"type":"direct","tag":"direct"}],"route":{"final":"direct"}}"#,
        )?;
        let result = self.check_config_file(binary, &config).await;
        let _ = fs::remove_file(&config);
        result
    }

    async fn check_config_file(&self, binary: &Path, config: &Path) -> Result<(), BinaryError> {
        let output = tokio::time::timeout(
            CHECK_TIMEOUT,
            tokio::process::Command::new(binary)
                .args(["check", "-c"])
                .arg(config)
                .kill_on_drop(true)
                .output(),
        )
        .await;
        let output = output
            .map_err(|_| BinaryError::Check("timed out".to_string()))?
            .map_err(BinaryError::Io)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(BinaryError::Check(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ))
        }
    }

    fn write_temp_binary(&self, bytes: &[u8]) -> Result<PathBuf, BinaryError> {
        fsutil::ensure_private_dir(&self.paths.bin_dir)?;
        let temp = self.paths.bin_dir.join("sing-box.new");
        let mut file = fsutil::open_private_file(&temp, false)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        set_executable(&temp)?;
        Ok(temp)
    }

    fn install_temp_binary(
        &self,
        temp: &Path,
        version: &str,
        source: &Path,
        sha256: Option<&str>,
        verified: bool,
    ) -> Result<(), BinaryError> {
        let target = self.paths.binary();
        if target.exists() {
            self.preserve_current_metadata();
            let _ = fs::remove_file(self.paths.previous_binary());
            fs::rename(&target, self.paths.previous_binary())?;
        }
        fs::rename(temp, &target)?;
        let metadata = BinaryMetadata {
            version: version.to_string(),
            source: source.display().to_string(),
            sha256: sha256.map(str::to_string),
            installed_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            verified,
        };
        self.write_metadata(&metadata)?;
        Ok(())
    }

    fn preserve_current_metadata(&self) {
        match fs::read_to_string(self.paths.binary_metadata()) {
            Ok(text) => {
                let _ = fsutil::write_private_atomic(
                    &self.paths.previous_binary_metadata(),
                    text.as_bytes(),
                );
            }
            Err(_) => {
                let _ = fs::remove_file(self.paths.previous_binary_metadata());
            }
        }
    }

    fn write_metadata(&self, metadata: &BinaryMetadata) -> Result<(), BinaryError> {
        let text = serde_json::to_string_pretty(metadata)
            .map_err(|err| BinaryError::Version(err.to_string()))?;
        fsutil::write_private_atomic(&self.paths.binary_metadata(), text.as_bytes())?;
        Ok(())
    }

    fn minimal_metadata(&self, version: &str) -> BinaryMetadata {
        BinaryMetadata {
            version: version.to_string(),
            source: "rollback".to_string(),
            sha256: None,
            installed_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            verified: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DownloadChannel {
    Direct,
    EnvProxy,
    LocalProxy,
}

impl DownloadChannel {
    fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::EnvProxy => "env-proxy",
            Self::LocalProxy => "local-proxy",
        }
    }
}

struct ChannelClient {
    channel: DownloadChannel,
    client: Client,
}

fn download_clients(config: &Config) -> Result<Vec<ChannelClient>, BinaryError> {
    let mut clients = vec![ChannelClient {
        channel: DownloadChannel::Direct,
        client: download_client()?,
    }];
    if let Some(url) = env_proxy_url() {
        if let Ok(client) = proxy_client(&url) {
            clients.push(ChannelClient {
                channel: DownloadChannel::EnvProxy,
                client,
            });
        }
    }
    let local_url = match config.lan_auth() {
        Some((username, password)) => {
            crate::netinfo::local_proxy_url_with_auth(config.port, username, password)
        }
        None => crate::netinfo::local_proxy_url(config.port),
    };
    if let Ok(client) = proxy_client(&local_url) {
        clients.push(ChannelClient {
            channel: DownloadChannel::LocalProxy,
            client,
        });
    }
    Ok(clients)
}

fn env_proxy_url() -> Option<String> {
    const KEYS: [&str; 6] = [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ];
    KEYS.iter().find_map(|key| {
        let value = std::env::var(key).ok()?;
        let value = value.trim();
        (!value.is_empty()).then(|| normalize_proxy_url(value))
    })
}

fn normalize_proxy_url(value: &str) -> String {
    if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    }
}

fn download_client() -> Result<Client, BinaryError> {
    Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .user_agent(format!("DFRelay/{}", crate::version::BASE))
        .build()
        .map_err(|err| BinaryError::Network(err.to_string()))
}

fn proxy_client(url: &str) -> Result<Client, BinaryError> {
    let proxy = Proxy::all(url).map_err(|err| BinaryError::Network(err.to_string()))?;
    Client::builder()
        .proxy(proxy)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .user_agent(format!("DFRelay/{}", crate::version::BASE))
        .build()
        .map_err(|err| BinaryError::Network(err.to_string()))
}

async fn resolve_latest_release(
    clients: &[ChannelClient],
    api: &str,
    logger: Option<&Logger>,
) -> Result<ReleaseAsset, BinaryError> {
    let mut last_error = BinaryError::Network("no network channel available".to_string());
    for channel in clients {
        let label = channel.channel.label();
        match tokio::time::timeout(
            UPDATE_CHECK_TIMEOUT,
            fetch_latest_release(&channel.client, api),
        )
        .await
        {
            Ok(Ok(asset)) => return Ok(asset),
            Ok(Err(err)) => {
                if let Some(logger) = logger {
                    logger.app(
                        LogLevel::Warn,
                        format!("release API attempt {label} failed: {err}"),
                    );
                }
                last_error = err;
            }
            Err(_) => {
                let err = BinaryError::Network(format!("{label} timed out"));
                if let Some(logger) = logger {
                    logger.app(
                        LogLevel::Warn,
                        format!("release API attempt {label} failed: {err}"),
                    );
                }
                last_error = err;
            }
        }
    }
    Err(last_error)
}

async fn fetch_latest_release(client: &Client, api: &str) -> Result<ReleaseAsset, BinaryError> {
    let response = client
        .get(api)
        .send()
        .await
        .map_err(|err| BinaryError::Network(err.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(BinaryError::Http(status.as_u16()));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|err| BinaryError::Network(err.to_string()))?;
    let version = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .trim_start_matches('v')
        .to_string();
    let asset = release_asset(&value, std::env::consts::OS, std::env::consts::ARCH)
        .ok_or(BinaryError::NoAsset)?;
    Ok(ReleaseAsset { version, ..asset })
}

fn release_asset(value: &serde_json::Value, os: &str, arch: &str) -> Option<ReleaseAsset> {
    let version = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .trim_start_matches('v')
        .to_string();
    let name = asset_name(&version, os, arch)?;
    let assets = value.get("assets")?.as_array()?;
    let asset = assets
        .iter()
        .find(|asset| asset.get("name").and_then(|v| v.as_str()) == Some(name.as_str()))?;
    let url = asset.get("browser_download_url")?.as_str()?.to_string();
    let digest = asset
        .get("digest")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(ReleaseAsset {
        version,
        name,
        url,
        digest,
    })
}

pub fn asset_name(version: &str, os: &str, arch: &str) -> Option<String> {
    let arch = match (os, arch) {
        ("linux", "x86_64") => "amd64",
        ("linux", "aarch64" | "arm64") => "arm64",
        ("macos", "x86_64") => "amd64",
        ("macos", "aarch64" | "arm64") => "arm64",
        ("android", "aarch64" | "arm64") => "arm64",
        _ => return None,
    };
    match os {
        "linux" => Some(format!("sing-box-{version}-linux-{arch}-musl.tar.gz")),
        "macos" => Some(format!("sing-box-{version}-darwin-{arch}.tar.gz")),
        "android" => Some(format!("sing-box-{version}-android-{arch}.tar.gz")),
        _ => None,
    }
}

pub fn mirror_url(prefix: &str, github_url: &str) -> String {
    format!("{}/{}", prefix.trim_end_matches('/'), github_url)
}

async fn download_and_verify(
    client: &Client,
    url: &str,
    expected_digest: &str,
) -> Result<Vec<u8>, BinaryError> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|err| BinaryError::Network(err.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(BinaryError::Http(status.as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| BinaryError::Network(err.to_string()))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_ASSET_BYTES {
            return Err(BinaryError::Archive("asset is too large".to_string()));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.starts_with(b"<") {
        return Err(BinaryError::Archive("asset looks like HTML".to_string()));
    }
    let actual = sha256_hex(&bytes);
    let expected = expected_digest.trim_start_matches("sha256:");
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(BinaryError::DigestMismatch {
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(bytes)
}

fn extract_binary_bytes(path: &Path, bytes: &[u8]) -> Result<Vec<u8>, BinaryError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        extract_tar_gz(bytes)
    } else if name.ends_with(".zip") {
        extract_zip(bytes)
    } else {
        Ok(bytes.to_vec())
    }
}

fn extract_tar_gz(bytes: &[u8]) -> Result<Vec<u8>, BinaryError> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut found: Option<Vec<u8>> = None;
    for entry in archive
        .entries()
        .map_err(|err| BinaryError::Archive(err.to_string()))?
    {
        let entry = entry.map_err(|err| BinaryError::Archive(err.to_string()))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|err| BinaryError::Archive(err.to_string()))?;
        if path.file_name().and_then(|name| name.to_str()) != Some("sing-box") {
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
            return Err(BinaryError::Archive("unsafe archive path".to_string()));
        }
        if entry.size() > MAX_MANUAL_BYTES {
            return Err(BinaryError::Archive(
                "archive entry is too large".to_string(),
            ));
        }
        if found.is_some() {
            return Err(BinaryError::Archive(
                "archive contains multiple sing-box files".to_string(),
            ));
        }
        let mut binary = Vec::new();
        entry
            .take(MAX_MANUAL_BYTES + 1)
            .read_to_end(&mut binary)
            .map_err(|err| BinaryError::Archive(err.to_string()))?;
        if binary.len() as u64 > MAX_MANUAL_BYTES {
            return Err(BinaryError::Archive(
                "archive entry is too large".to_string(),
            ));
        }
        found = Some(binary);
    }
    found.ok_or_else(|| BinaryError::Archive("sing-box not found in archive".to_string()))
}

fn extract_zip(bytes: &[u8]) -> Result<Vec<u8>, BinaryError> {
    let reader = Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|err| BinaryError::Archive(err.to_string()))?;
    let mut found: Option<Vec<u8>> = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|err| BinaryError::Archive(err.to_string()))?;
        let Some(path) = entry.enclosed_name() else {
            return Err(BinaryError::Archive("unsafe zip path".to_string()));
        };
        if path.file_name().and_then(|name| name.to_str()) != Some("sing-box") {
            continue;
        }
        if !entry.is_file() {
            continue;
        }
        if entry.size() > MAX_MANUAL_BYTES {
            return Err(BinaryError::Archive("zip entry is too large".to_string()));
        }
        if found.is_some() {
            return Err(BinaryError::Archive(
                "archive contains multiple sing-box files".to_string(),
            ));
        }
        let mut binary = Vec::new();
        entry
            .take(MAX_MANUAL_BYTES + 1)
            .read_to_end(&mut binary)
            .map_err(|err| BinaryError::Archive(err.to_string()))?;
        if binary.len() as u64 > MAX_MANUAL_BYTES {
            return Err(BinaryError::Archive("zip entry is too large".to_string()));
        }
        found = Some(binary);
    }
    found.ok_or_else(|| BinaryError::Archive("sing-box not found in archive".to_string()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_version_output(output: &str) -> Option<String> {
    let line = output.lines().next()?.trim();
    let mut parts = line.split_whitespace();
    if parts.next()? != "sing-box" || parts.next()? != "version" {
        return None;
    }
    parts.next().map(str::to_string)
}

pub fn is_newer_version(latest: &str, current: &str) -> bool {
    compare_versions(latest, current) == std::cmp::Ordering::Greater
}

fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let (left_numbers, left_prerelease) = version_parts(left);
    let (right_numbers, right_prerelease) = version_parts(right);
    let length = left_numbers.len().max(right_numbers.len());
    for index in 0..length {
        let left = left_numbers.get(index).copied().unwrap_or(0);
        let right = right_numbers.get(index).copied().unwrap_or(0);
        match left.cmp(&right) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    match (left_prerelease, right_prerelease) {
        (false, true) => std::cmp::Ordering::Greater,
        (true, false) => std::cmp::Ordering::Less,
        _ => std::cmp::Ordering::Equal,
    }
}

fn version_parts(version: &str) -> (Vec<u64>, bool) {
    let without_build = version.trim().trim_start_matches('v');
    let version = without_build.split('+').next().unwrap_or(without_build);
    let (core, prerelease) = version
        .split_once('-')
        .map_or((version, false), |(core, _)| (core, true));
    let numbers = core
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect();
    (numbers, prerelease)
}

fn is_supported_manual_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".zip")
        || lower == "sing-box"
        || lower.starts_with("sing-box-")
}

fn read_sidecar_checksum(path: &Path) -> Result<Option<String>, BinaryError> {
    let sidecar = PathBuf::from(format!("{}.sha256", path.display()));
    if !sidecar.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(sidecar)?;
    let token = text
        .split_whitespace()
        .next()
        .ok_or_else(|| BinaryError::Archive("empty sha256 file".to_string()))?;
    if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(BinaryError::Archive("invalid sha256 file".to_string()));
    }
    Ok(Some(token.to_string()))
}

fn set_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn asset_names_cover_release_targets() {
        assert_eq!(
            asset_name("1.14.4", "linux", "x86_64").unwrap(),
            "sing-box-1.14.4-linux-amd64-musl.tar.gz"
        );
        assert_eq!(
            asset_name("1.14.4", "linux", "aarch64").unwrap(),
            "sing-box-1.14.4-linux-arm64-musl.tar.gz"
        );
        assert_eq!(
            asset_name("1.14.4", "macos", "x86_64").unwrap(),
            "sing-box-1.14.4-darwin-amd64.tar.gz"
        );
        assert_eq!(
            asset_name("1.14.4", "macos", "aarch64").unwrap(),
            "sing-box-1.14.4-darwin-arm64.tar.gz"
        );
        assert_eq!(
            asset_name("1.14.4", "android", "aarch64").unwrap(),
            "sing-box-1.14.4-android-arm64.tar.gz"
        );
        assert_eq!(
            asset_name("1.14.4", "android", "arm64").unwrap(),
            "sing-box-1.14.4-android-arm64.tar.gz"
        );
        assert!(asset_name("1.14.4", "android", "x86_64").is_none());
        assert!(asset_name("1.14.4", "windows", "x86_64").is_none());
        assert!(asset_name("1.14.4", "linux", "armv7").is_none());
    }

    #[test]
    fn mirror_url_does_not_double_slash() {
        assert_eq!(
            mirror_url("https://gh-proxy.com/", "https://github.com/x/y"),
            "https://gh-proxy.com/https://github.com/x/y"
        );
        assert_eq!(
            mirror_url("https://gh-proxy.com", "https://github.com/x/y"),
            "https://gh-proxy.com/https://github.com/x/y"
        );
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parses_version_output() {
        assert_eq!(
            parse_version_output("sing-box version 1.14.4\n\nEnvironment: ..."),
            Some("1.14.4".to_string())
        );
        assert!(parse_version_output("garbage").is_none());
    }

    #[test]
    fn compares_versions_for_update_checks() {
        assert!(is_newer_version("1.15.0", "1.14.4"));
        assert!(is_newer_version("v1.15.0", "v1.14.4"));
        assert!(is_newer_version("1.14.10", "1.14.9"));
        assert!(is_newer_version("1.14.4", "1.14.4-alpha.13"));
        assert!(is_newer_version("1.14", "1.13.9"));
        assert!(!is_newer_version("1.14.4", "1.14.4"));
        assert!(!is_newer_version("1.14.4-alpha.13", "1.14.4"));
        assert!(!is_newer_version("1.14.4", "1.15.0"));
        assert!(!is_newer_version("1.14.4+build", "1.14.4"));
    }

    #[test]
    fn extracts_tar_gz_sing_box() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            let data = b"#!/bin/sh\n";
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "sing-box-1.14.4/sing-box", &data[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            let mut encoder =
                flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
            encoder.write_all(&tar_bytes).unwrap();
            encoder.finish().unwrap();
        }
        assert_eq!(extract_tar_gz(&gz).unwrap(), b"#!/bin/sh\n");
    }

    #[test]
    fn archive_without_sing_box_is_error() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            let data = b"hello";
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "readme.txt", &data[..])
                .unwrap();
            builder.finish().unwrap();
        }
        assert!(extract_tar_gz(&tar_bytes).is_err());
    }

    fn zip_bytes(entries: &[(&str, &[u8], Option<u32>)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut bytes));
            for (name, data, mode) in entries {
                let mut options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                if let Some(mode) = mode {
                    options = options.external_attributes((*mode) << 16);
                }
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    #[test]
    fn extracts_zip_sing_box() {
        let bytes = zip_bytes(&[("sing-box-1.14.4/sing-box", b"#!/bin/sh\n", None)]);
        assert_eq!(extract_zip(&bytes).unwrap(), b"#!/bin/sh\n");
    }

    #[test]
    fn rejects_unsafe_zip_paths() {
        let bytes = zip_bytes(&[("../sing-box", b"payload", None)]);
        assert!(extract_zip(&bytes).is_err());
    }

    #[test]
    fn rejects_duplicate_zip_sing_box_entries() {
        let bytes = zip_bytes(&[
            ("a/sing-box", b"first", None),
            ("b/sing-box", b"second", None),
        ]);
        assert!(extract_zip(&bytes).is_err());
    }

    #[test]
    fn skips_zip_symlink_entries() {
        let bytes = zip_bytes(&[("sing-box", b"/etc/passwd", Some(0o120777))]);
        assert!(extract_zip(&bytes).is_err());
    }

    #[tokio::test]
    async fn installs_sing_box_from_zip_inbox() {
        let dir = TempDir::new().unwrap();
        let paths = Paths {
            config_file: dir.path().join("config.toml"),
            data_dir: dir.path().to_path_buf(),
            bin_dir: dir.path().join("bin"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: dir.path().join("runtime"),
            logs_dir: dir.path().join("logs"),
            inbox_dir: dir.path().join("inbox"),
        };
        paths.ensure().unwrap();
        let manager = BinaryManager::new(paths.clone());
        let logger = Logger::new(&paths.logs_dir, LogLevel::Info, LogLevel::Info).unwrap();
        let script = fake_binary_script("1.14.4");
        let bytes = zip_bytes(&[("sing-box", script.as_bytes(), None)]);
        let archive = paths.inbox_dir.join("sing-box-1.14.4.zip");
        fs::write(&archive, bytes).unwrap();

        let metadata = manager.install_from_inbox(&archive, &logger).await.unwrap();
        assert_eq!(metadata.version, "1.14.4");
        assert_eq!(manager.version(&paths.binary()).await.unwrap(), "1.14.4");
    }

    #[test]
    fn upgrading_keeps_only_the_previous_binary_for_rollback() {
        let dir = TempDir::new().unwrap();
        let paths = Paths {
            config_file: dir.path().join("config.toml"),
            data_dir: dir.path().to_path_buf(),
            bin_dir: dir.path().join("bin"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: dir.path().join("runtime"),
            logs_dir: dir.path().join("logs"),
            inbox_dir: dir.path().join("inbox"),
        };
        paths.ensure().unwrap();
        let manager = BinaryManager::new(paths.clone());
        fs::write(paths.binary(), b"v1").unwrap();

        let temp = manager.write_temp_binary(b"v2").unwrap();
        manager
            .install_temp_binary(&temp, "1.15.0", Path::new("test"), None, true)
            .unwrap();
        assert_eq!(fs::read(paths.previous_binary()).unwrap(), b"v1");
        assert_eq!(fs::read(paths.binary()).unwrap(), b"v2");

        let temp = manager.write_temp_binary(b"v3").unwrap();
        manager
            .install_temp_binary(&temp, "1.16.0", Path::new("test"), None, true)
            .unwrap();
        assert_eq!(fs::read(paths.previous_binary()).unwrap(), b"v2");
        assert_eq!(fs::read(paths.binary()).unwrap(), b"v3");
    }

    fn fake_binary_script(version: &str) -> String {
        format!(
            "#!/bin/sh\nif [ \"$1\" = version ]; then echo \"sing-box version {version}\"; exit 0; fi\nexit 0\n"
        )
    }

    fn write_fake_binary(path: &Path, version: &str) {
        fs::write(path, fake_binary_script(version)).unwrap();
        set_executable(path).unwrap();
    }

    fn test_metadata(version: &str) -> BinaryMetadata {
        BinaryMetadata {
            version: version.to_string(),
            source: "test".to_string(),
            sha256: None,
            installed_at: "2026-11-01 00:00:00".to_string(),
            verified: true,
        }
    }

    #[tokio::test]
    async fn rollback_swaps_binaries_and_metadata() {
        let dir = TempDir::new().unwrap();
        let paths = Paths {
            config_file: dir.path().join("config.toml"),
            data_dir: dir.path().to_path_buf(),
            bin_dir: dir.path().join("bin"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: dir.path().join("runtime"),
            logs_dir: dir.path().join("logs"),
            inbox_dir: dir.path().join("inbox"),
        };
        paths.ensure().unwrap();
        let manager = BinaryManager::new(paths.clone());
        let logger = Logger::new(&paths.logs_dir, LogLevel::Info, LogLevel::Info).unwrap();
        write_fake_binary(&paths.binary(), "1.15.0");
        write_fake_binary(&paths.previous_binary(), "1.14.4");
        fs::write(
            paths.binary_metadata(),
            serde_json::to_string(&test_metadata("1.15.0")).unwrap(),
        )
        .unwrap();
        fs::write(
            paths.previous_binary_metadata(),
            serde_json::to_string(&test_metadata("1.14.4")).unwrap(),
        )
        .unwrap();

        let metadata = manager.rollback(&logger).await.unwrap();
        assert_eq!(metadata.version, "1.14.4");
        assert_eq!(manager.metadata().unwrap().version, "1.14.4");
        assert_eq!(manager.previous_metadata().unwrap().version, "1.15.0");
        let output = std::process::Command::new(paths.binary())
            .arg("version")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("1.14.4"));

        let metadata = manager.rollback(&logger).await.unwrap();
        assert_eq!(metadata.version, "1.15.0");
        assert_eq!(manager.metadata().unwrap().version, "1.15.0");
        assert_eq!(manager.previous_metadata().unwrap().version, "1.14.4");
    }

    #[tokio::test]
    async fn upgrade_check_covers_the_existing_runtime_config() {
        let dir = TempDir::new().unwrap();
        let paths = Paths {
            config_file: dir.path().join("config.toml"),
            data_dir: dir.path().to_path_buf(),
            bin_dir: dir.path().join("bin"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: dir.path().join("runtime"),
            logs_dir: dir.path().join("logs"),
            inbox_dir: dir.path().join("inbox"),
        };
        paths.ensure().unwrap();
        let binary = paths.bin_dir.join("fake-sing-box");
        let script = "#!/bin/sh\nif [ \"$1\" = check ]; then\n  case \"$3\" in *runtime/config.json*) exit 1;; esac\nfi\nexit 0\n";
        fs::write(&binary, script).unwrap();
        set_executable(&binary).unwrap();
        fs::write(paths.runtime_config(), "{}").unwrap();

        let manager = BinaryManager::new(paths);
        assert!(manager.check_binary(&binary).await.is_ok());
        assert!(manager.check_existing_config(&binary).await.is_err());
    }

    #[test]
    fn inbox_scan_ignores_checksum_files() {
        let dir = TempDir::new().unwrap();
        let paths = Paths {
            config_file: dir.path().join("config.toml"),
            data_dir: dir.path().to_path_buf(),
            bin_dir: dir.path().join("bin"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: dir.path().join("runtime"),
            logs_dir: dir.path().join("logs"),
            inbox_dir: dir.path().join("inbox"),
        };
        paths.ensure().unwrap();
        fs::write(paths.inbox_dir.join("sing-box-1.14.4.tar.gz"), b"x").unwrap();
        fs::write(paths.inbox_dir.join("sing-box-1.14.4.tar.gz.sha256"), "abc").unwrap();
        fs::write(paths.inbox_dir.join("readme.txt"), "x").unwrap();
        let manager = BinaryManager::new(paths);
        let candidates = manager.scan_inbox().unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].has_checksum);
    }

    #[test]
    fn proxy_urls_without_a_scheme_default_to_http() {
        assert_eq!(
            normalize_proxy_url("127.0.0.1:7890"),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            normalize_proxy_url("https://proxy.example:8443"),
            "https://proxy.example:8443"
        );
    }

    #[test]
    fn download_channels_try_direct_then_proxies() {
        let clients = download_clients(&Config::default()).unwrap();
        let channels: Vec<_> = clients.iter().map(|client| client.channel).collect();
        assert_eq!(channels.first(), Some(&DownloadChannel::Direct));
        assert_eq!(channels.last(), Some(&DownloadChannel::LocalProxy));
        if let Some(env) = channels
            .iter()
            .position(|channel| *channel == DownloadChannel::EnvProxy)
        {
            assert!(env < channels.len() - 1);
        }
    }
}
