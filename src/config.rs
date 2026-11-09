use std::ffi::OsStr;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::i18n::Lang;
use crate::logging::LogLevel;

pub const DEFAULT_PORT: u16 = 12000;
pub const DEFAULT_TEST_URL: &str = "https://www.gstatic.com/generate_204";
pub const DEFAULT_TEST_INTERVAL: &str = "3m";
pub const DEFAULT_IDLE_TIMEOUT: &str = "30m";
pub const DEFAULT_DNS_SERVERS: [&str; 2] = ["1.1.1.1", "8.8.8.8"];

/// Accept the Go `time.ParseDuration` subset used by sing-box: a sequence of
/// decimal numbers with ns/us/µs/μs/ms/s/m/h units. Negative values are not
/// meaningful for these settings.
fn is_go_duration(value: &str) -> bool {
    let mut rest = value.trim();
    if rest.is_empty() {
        return false;
    }
    let mut found = false;
    while !rest.is_empty() {
        let number_len = rest
            .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
            .unwrap_or(rest.len());
        if number_len == 0 {
            return false;
        }
        let number = &rest[..number_len];
        if number.matches('.').count() > 1 || !number.chars().any(|ch| ch.is_ascii_digit()) {
            return false;
        }
        rest = &rest[number_len..];
        let unit_len = rest
            .find(|ch: char| ch.is_ascii_digit() || ch == '.')
            .unwrap_or(rest.len());
        let unit = &rest[..unit_len];
        if !matches!(unit, "ns" | "us" | "µs" | "μs" | "ms" | "s" | "m" | "h") {
            return false;
        }
        rest = &rest[unit_len..];
        found = true;
    }
    found
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub language: Lang,
    pub port: u16,
    pub enable_ipv6: bool,
    pub bypass_lan: bool,
    pub allow_lan: bool,
    pub lan_username: String,
    pub lan_password: String,
    pub dns_servers: Vec<String>,
    pub test_url: String,
    pub test_interval: String,
    pub tolerance_ms: u16,
    pub idle_timeout: String,
    pub keywords_include: Vec<String>,
    pub keywords_exclude: Vec<String>,
    pub subscription_refresh_hours: u64,
    pub app_log_level: LogLevel,
    pub singbox_log_level: LogLevel,
    pub github_mirrors: Vec<String>,
    pub proxy_ip_v4_url: String,
    pub proxy_ip_v6_url: String,
    pub subscriptions: Vec<SubscriptionConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionConfig {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Config {
    pub fn load_or_default(path: &Path) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        if !path.exists() {
            let config = Self::default();
            if let Err(err) = config.save(path) {
                warnings.push(format!("write default config failed: {err}"));
            }
            return (config, warnings);
        }

        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => warnings
                .push("config file is a symlink; its permissions were not changed".to_string()),
            Ok(metadata) if metadata.is_file() => {
                if let Err(err) = fsutil::enforce_private_file(path) {
                    warnings.push(format!("cannot enforce 0600 on config file: {err}"));
                }
            }
            Ok(_) => {}
            Err(err) => warnings.push(format!("cannot inspect config file permissions: {err}")),
        }

        let mut config = match fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(config) => config,
                Err(err) => {
                    warnings.push(format!(
                        "parse config failed: {err}; moved invalid file to {} and using defaults",
                        backup_invalid_config(path)
                            .map(|backup| backup.display().to_string())
                            .unwrap_or_else(|| "(backup failed)".to_string())
                    ));
                    Self::default()
                }
            },
            Err(err) => {
                warnings.push(format!("read config failed, using defaults: {err}"));
                Self::default()
            }
        };
        warnings.extend(config.validate());
        (config, warnings)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fsutil::ensure_private_dir(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        fsutil::write_private_atomic(path, text.as_bytes())?;
        Ok(())
    }

    pub fn validate(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.port == 0 {
            self.port = DEFAULT_PORT;
            warnings.push(format!("port 0 reset to {DEFAULT_PORT}"));
        }
        self.lan_username = self.lan_username.trim().to_string();
        if self.lan_username.contains(':') {
            self.lan_username.clear();
            warnings.push("LAN username must not contain ':'; cleared".to_string());
        }
        if self.allow_lan && self.lan_username.is_empty() != self.lan_password.is_empty() {
            warnings.push(
                "LAN access is enabled but username/password is incomplete; no authentication"
                    .to_string(),
            );
        }
        if !self.test_url.starts_with("https://") {
            self.test_url = DEFAULT_TEST_URL.to_string();
            warnings.push("test URL must be https; reset to default".to_string());
        }
        if !is_go_duration(&self.test_interval) {
            self.test_interval = DEFAULT_TEST_INTERVAL.to_string();
            warnings.push("test interval is not a valid duration; reset to default".to_string());
        }
        if !is_go_duration(&self.idle_timeout) {
            self.idle_timeout = DEFAULT_IDLE_TIMEOUT.to_string();
            warnings.push("idle timeout is not a valid duration; reset to default".to_string());
        }
        let dns_servers = clean_dns_servers(&self.dns_servers);
        if !self.dns_servers.is_empty() && dns_servers != self.dns_servers {
            warnings.push("DNS servers must be IP literals; invalid entries removed".to_string());
        }
        self.dns_servers = dns_servers;
        self.keywords_include = clean_keywords(&self.keywords_include);
        self.keywords_exclude = clean_keywords(&self.keywords_exclude);
        self.github_mirrors = self
            .github_mirrors
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| s.starts_with("https://"))
            .collect();
        let before = self.subscriptions.len();
        self.subscriptions.retain(|s| {
            !s.url.trim().is_empty()
                && (s.url.starts_with("https://") || s.url.starts_with("http://"))
        });
        if self.subscriptions.len() != before {
            warnings.push("subscriptions with invalid URLs were removed".to_string());
        }
        let mut seen_ids = std::collections::HashSet::new();
        let before = self.subscriptions.len();
        self.subscriptions.retain(|subscription| {
            !subscription.id.is_empty() && seen_ids.insert(subscription.id.clone())
        });
        if self.subscriptions.len() != before {
            warnings.push("subscriptions with empty or duplicate IDs were removed".to_string());
        }
        warnings
    }

    pub fn enabled_subscriptions(&self) -> impl Iterator<Item = &SubscriptionConfig> {
        self.subscriptions.iter().filter(|s| s.enabled)
    }

    pub fn lan_auth(&self) -> Option<(&str, &str)> {
        if self.allow_lan && !self.lan_username.is_empty() && !self.lan_password.is_empty() {
            Some((&self.lan_username, &self.lan_password))
        } else {
            None
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: Lang::Zh,
            port: DEFAULT_PORT,
            enable_ipv6: true,
            bypass_lan: true,
            allow_lan: false,
            lan_username: String::new(),
            lan_password: String::new(),
            dns_servers: DEFAULT_DNS_SERVERS
                .iter()
                .map(|server| server.to_string())
                .collect(),
            test_url: DEFAULT_TEST_URL.to_string(),
            test_interval: DEFAULT_TEST_INTERVAL.to_string(),
            tolerance_ms: 50,
            idle_timeout: DEFAULT_IDLE_TIMEOUT.to_string(),
            keywords_include: Vec::new(),
            keywords_exclude: Vec::new(),
            subscription_refresh_hours: 24,
            app_log_level: LogLevel::Info,
            singbox_log_level: LogLevel::Info,
            github_mirrors: default_mirrors(),
            proxy_ip_v4_url: "https://api.ipify.org".to_string(),
            proxy_ip_v6_url: "https://api6.ipify.org".to_string(),
            subscriptions: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_mirrors() -> Vec<String> {
    [
        "https://gh-proxy.com/",
        "https://ghfast.top/",
        "https://ghproxy.net/",
        "https://ghproxy.homeboyc.cn/",
        "https://gh.zwy.one/",
        "https://github.moeyy.xyz/",
        "https://hub.gitmirror.com/",
        "https://gh.llkk.cc/",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn xdg_dir(value: Option<&OsStr>, home: &Path, fallback: &str) -> PathBuf {
    match value {
        Some(value) if !value.is_empty() => {
            let path = Path::new(value);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                home.join(fallback)
            }
        }
        _ => home.join(fallback),
    }
}

fn clean_dns_servers(values: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim();
        if value.parse::<IpAddr>().is_ok() && !out.iter().any(|item| item == value) {
            out.push(value.to_string());
        }
    }
    if out.is_empty() {
        out = DEFAULT_DNS_SERVERS
            .iter()
            .map(|server| server.to_string())
            .collect();
    }
    out
}

fn clean_keywords(values: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !out.iter().any(|item| item == value) {
            out.push(value.to_string());
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub inbox_dir: PathBuf,
}

impl Paths {
    pub fn discover(
        config_override: Option<PathBuf>,
        data_override: Option<PathBuf>,
    ) -> Result<Self, ConfigError> {
        let base = BaseDirs::new().ok_or(ConfigError::NoHome)?;
        let home = base.home_dir();
        let config_home = xdg_dir(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            home,
            ".config",
        );
        let data_home = xdg_dir(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            home,
            ".local/share",
        );
        let app_data = match data_override {
            Some(path) => std::path::absolute(path)?,
            None => data_home.join("dfrelay"),
        };
        let config_file = match config_override {
            Some(path) => std::path::absolute(path)?,
            None => config_home.join("dfrelay/config.toml"),
        };
        Ok(Self {
            config_file,
            bin_dir: app_data.join("bin"),
            cache_dir: app_data.join("cache"),
            runtime_dir: app_data.join("runtime"),
            logs_dir: app_data.join("logs"),
            inbox_dir: app_data.join("inbox"),
            data_dir: app_data,
        })
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            &self.data_dir,
            &self.bin_dir,
            &self.cache_dir,
            &self.runtime_dir,
            &self.logs_dir,
            &self.inbox_dir,
        ] {
            fsutil::ensure_private_dir(dir)?;
        }
        if let Some(parent) = self
            .config_file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fsutil::ensure_private_dir(parent)?;
        }
        Ok(())
    }

    pub fn binary(&self) -> PathBuf {
        self.bin_dir.join("sing-box")
    }

    pub fn previous_binary(&self) -> PathBuf {
        self.bin_dir.join("sing-box.previous")
    }

    pub fn previous_binary_metadata(&self) -> PathBuf {
        self.bin_dir.join("previous.json")
    }

    pub fn binary_metadata(&self) -> PathBuf {
        self.bin_dir.join("version.json")
    }

    pub fn runtime_config(&self) -> PathBuf {
        self.runtime_dir.join("config.json")
    }

    pub fn cache_file(&self) -> PathBuf {
        self.runtime_dir.join("cache.db")
    }

    pub fn subscription_cache(&self) -> PathBuf {
        self.cache_dir.join("subscriptions.json")
    }

    pub fn control_socket(&self) -> PathBuf {
        self.runtime_dir.join("control.sock")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot determine home directory")]
    NoHome,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

fn backup_invalid_config(path: &Path) -> Option<PathBuf> {
    let file_name = path.file_name()?.to_string_lossy();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let backup = path.with_file_name(format!("{file_name}.invalid-{stamp}"));
    fs::rename(path, &backup).ok()?;
    // A config symlink is backed up as a symlink; do not chmod its target.
    if fs::symlink_metadata(&backup)
        .map(|metadata| !metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        let _ = fsutil::enforce_private_file(&backup);
    }
    Some(backup)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn defaults_are_chinese_and_safe() {
        let config = Config::default();
        assert_eq!(config.language, Lang::Zh);
        assert_eq!(config.port, 12000);
        assert!(config.bypass_lan);
        assert!(config.enable_ipv6);
        assert!(!config.allow_lan);
        assert!(config.lan_username.is_empty());
        assert!(config.lan_password.is_empty());
        assert_eq!(config.dns_servers, vec!["1.1.1.1", "8.8.8.8"]);
        assert!(!config.github_mirrors.is_empty());
    }

    #[test]
    fn legacy_singbox_path_is_ignored() {
        let config: Config = toml::from_str("singbox_path = \"/opt/sing-box\"\n").unwrap();
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.language, Lang::Zh);
    }

    #[test]
    fn toml_roundtrip_preserves_settings() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.port = 12345;
        config.language = Lang::En;
        config.keywords_include = vec!["香港".into(), "HK".into()];
        config.bypass_lan = false;
        config.allow_lan = true;
        config.lan_username = "alice".into();
        config.lan_password = "s3cret".into();
        config.save(&path).unwrap();
        let (loaded, warnings) = Config::load_or_default(&path);
        assert!(warnings.is_empty());
        assert_eq!(loaded.port, 12345);
        assert_eq!(loaded.language, Lang::En);
        assert_eq!(loaded.keywords_include, vec!["香港", "HK"]);
        assert!(!loaded.bypass_lan);
        assert_eq!(loaded.lan_auth(), Some(("alice", "s3cret")));
    }

    #[test]
    fn go_duration_validation_accepts_sing_box_values() {
        for value in ["0s", "300ms", "3m", "1h30m", "1.5h", ".5s", "20µs"] {
            assert!(is_go_duration(value), "{value}");
        }
        for value in ["", "3", "3x", "-3s", "1.2.3s", "3m 1s", "h"] {
            assert!(!is_go_duration(value), "{value}");
        }
    }

    #[test]
    fn validation_cleans_invalid_values() {
        let mut config = Config::default();
        config.port = 0;
        config.test_url = "http://example.com".into();
        config.test_interval = "3x".into();
        config.idle_timeout = "".into();
        config.keywords_include = vec![" 香港 ".into(), "".into(), "香港".into()];
        config.github_mirrors = vec!["http://bad".into(), "https://ok/".into()];
        config.dns_servers = vec![
            "1.1.1.1".into(),
            "not-an-ip".into(),
            "1.1.1.1".into(),
            "8.8.8.8".into(),
        ];
        let warnings = config.validate();
        assert!(!warnings.is_empty());
        assert_eq!(config.port, 12000);
        assert_eq!(config.test_url, DEFAULT_TEST_URL);
        assert_eq!(config.test_interval, DEFAULT_TEST_INTERVAL);
        assert_eq!(config.idle_timeout, DEFAULT_IDLE_TIMEOUT);
        assert_eq!(config.keywords_include, vec!["香港"]);
        assert_eq!(config.github_mirrors, vec!["https://ok/"]);
        assert_eq!(config.dns_servers, vec!["1.1.1.1", "8.8.8.8"]);
    }

    #[test]
    fn lan_credentials_are_optional_and_username_is_sanitized() {
        let mut config = Config::default();
        config.allow_lan = true;
        config.lan_username = " alice ".into();
        config.lan_password = "s3cret".into();
        assert!(config.validate().is_empty());
        assert_eq!(config.lan_auth(), Some(("alice", "s3cret")));

        config.lan_username = "bad:name".into();
        let warnings = config.validate();
        assert!(warnings.iter().any(|warning| warning.contains("':'")));
        assert!(config.lan_username.is_empty());
        assert!(config.lan_auth().is_none());

        let mut config = Config::default();
        config.allow_lan = true;
        config.lan_username = "alice".into();
        let warnings = config.validate();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("no authentication")),
            "{warnings:?}"
        );
        assert!(config.lan_auth().is_none());
    }

    #[test]
    fn xdg_dir_prefers_absolute_env_and_falls_back() {
        let home = Path::new("/home/tester");
        assert_eq!(
            xdg_dir(Some(OsStr::new("/tmp/xdg-config")), home, ".config"),
            PathBuf::from("/tmp/xdg-config")
        );
        assert_eq!(
            xdg_dir(Some(OsStr::new("relative/path")), home, ".config"),
            home.join(".config")
        );
        assert_eq!(
            xdg_dir(Some(OsStr::new("")), home, ".config"),
            home.join(".config")
        );
        assert_eq!(xdg_dir(None, home, ".config"), home.join(".config"));
        assert_eq!(
            xdg_dir(None, home, ".local/share"),
            home.join(".local/share")
        );
    }

    #[test]
    fn invalid_config_is_backed_up_before_defaults_are_used() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "this is not toml [[[").unwrap();
        let (config, warnings) = Config::load_or_default(&path);
        assert_eq!(config.port, DEFAULT_PORT);
        assert!(!path.exists());
        let backup = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().contains(".invalid-"))
            .expect("invalid config backup");
        assert!(fs::read_to_string(backup).unwrap().contains("not toml"));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("moved invalid file")));
    }

    #[test]
    fn duplicate_subscription_ids_are_pruned() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.subscriptions = vec![
            SubscriptionConfig {
                id: "same".into(),
                name: "one".into(),
                url: "https://example.com/one".into(),
                enabled: true,
            },
            SubscriptionConfig {
                id: "same".into(),
                name: "two".into(),
                url: "https://example.com/two".into(),
                enabled: true,
            },
        ];
        config.save(&path).unwrap();
        let (loaded, warnings) = Config::load_or_default(&path);
        assert_eq!(loaded.subscriptions.len(), 1);
        assert_eq!(loaded.subscriptions[0].name, "one");
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("duplicate IDs")));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_keeps_existing_directory_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let shared = dir.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        let paths =
            Paths::discover(Some(shared.join("config.toml")), Some(shared.join("data"))).unwrap();
        paths.ensure().unwrap();
        let mode = fs::metadata(&shared).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn missing_dns_servers_uses_defaults_without_warning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "port = 12000\n").unwrap();
        let (config, warnings) = Config::load_or_default(&path);
        assert!(warnings.is_empty());
        assert_eq!(config.dns_servers, vec!["1.1.1.1", "8.8.8.8"]);
    }

    #[cfg(unix)]
    #[test]
    fn existing_config_and_invalid_backup_are_tightened_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "port = 12000\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let (_config, warnings) = Config::load_or_default(&path);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::write(&path, "not toml [[[").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let _ = Config::load_or_default(&path);
        let backup = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|candidate| candidate.to_string_lossy().contains(".invalid-"))
            .expect("invalid backup");
        assert_eq!(
            fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn backing_up_a_symlinked_invalid_config_does_not_chmod_the_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("shared.toml");
        fs::write(&target, "not toml [[[").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let link = dir.path().join("config.toml");
        symlink(&target, &link).unwrap();
        let (_, warnings) = Config::load_or_default(&link);
        assert!(warnings.iter().any(|warning| warning.contains("symlink")));
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn paths_use_explicit_overrides() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("custom.toml");
        let data = dir.path().join("data");
        let paths = Paths::discover(Some(config_path.clone()), Some(data.clone())).unwrap();
        assert_eq!(paths.config_file, config_path);
        assert_eq!(paths.data_dir, data);
        assert_eq!(paths.inbox_dir, data.join("inbox"));
        paths.ensure().unwrap();
        assert!(paths.inbox_dir.is_dir());
    }

    #[test]
    fn relative_overrides_are_made_absolute() {
        let paths = Paths::discover(
            Some(PathBuf::from("relative-config.toml")),
            Some(PathBuf::from("relative-data")),
        )
        .unwrap();
        assert!(paths.config_file.is_absolute());
        assert!(paths.data_dir.is_absolute());
    }

    #[test]
    fn legacy_app_update_key_is_ignored() {
        let config: Config = toml::from_str("app_update = \"auto\"").unwrap();
        assert_eq!(config.language, Lang::Zh);
    }
}
