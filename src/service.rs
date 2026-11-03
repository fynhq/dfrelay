use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::fsutil;

const MARKER_FILE: &str = "service.json";
const MARKER_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceManager {
    LaunchdGui,
    LaunchdSystem,
    SystemdUser,
    SystemdSystem,
}

impl ServiceManager {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "launchd-gui" => Some(Self::LaunchdGui),
            "launchd-system" => Some(Self::LaunchdSystem),
            "systemd-user" => Some(Self::SystemdUser),
            "systemd-system" => Some(Self::SystemdSystem),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LaunchdGui => "launchd-gui",
            Self::LaunchdSystem => "launchd-system",
            Self::SystemdUser => "systemd-user",
            Self::SystemdSystem => "systemd-system",
        }
    }

    fn is_user_scope(self) -> bool {
        matches!(self, Self::LaunchdGui | Self::SystemdUser)
    }

    fn platform_supported(self) -> bool {
        match self {
            Self::LaunchdGui | Self::LaunchdSystem => cfg!(target_os = "macos"),
            Self::SystemdUser | Self::SystemdSystem => cfg!(target_os = "linux"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceIdentity {
    pub manager: ServiceManager,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
    pub uid: u32,
}

impl ServiceIdentity {
    /// Parse the service identity from the environment of `dfr serve`.
    ///
    /// `DFRELAY_SERVICE=1` alone keeps the legacy behavior: the daemon is
    /// service-managed, but automatic supervisor control is unavailable.
    pub fn from_env() -> Result<Option<Self>, String> {
        if std::env::var("DFRELAY_SERVICE").as_deref() != Ok("1") {
            return Ok(None);
        }
        let manager_value = std::env::var("DFRELAY_SERVICE_MANAGER").unwrap_or_default();
        let unit_value = std::env::var("DFRELAY_SERVICE_UNIT").unwrap_or_default();
        let manager_value = manager_value.trim();
        let unit = unit_value.trim();
        if manager_value.is_empty() && unit.is_empty() {
            return Ok(None);
        }
        if manager_value.is_empty() {
            return Err(
                "DFRELAY_SERVICE_MANAGER is required when DFRELAY_SERVICE_UNIT is set".into(),
            );
        }
        let manager = ServiceManager::parse(manager_value)
            .ok_or_else(|| format!("unsupported DFRELAY_SERVICE_MANAGER `{manager_value}`"))?;
        if unit.is_empty() {
            return Err("DFRELAY_SERVICE_UNIT is required".into());
        }
        let target = match std::env::var_os("DFRELAY_SERVICE_TARGET") {
            Some(value) if !value.is_empty() => {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err("DFRELAY_SERVICE_TARGET must be an absolute path".into());
                }
                Some(path)
            }
            _ => None,
        };
        let identity = Self {
            manager,
            unit: unit.to_string(),
            target,
            uid: current_uid(),
        };
        identity.validate()?;
        Ok(Some(identity))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.unit.is_empty()
            || self.unit == "."
            || self.unit == ".."
            || self.unit.starts_with('-')
        {
            return Err("service unit is empty or invalid".into());
        }
        if !self
            .unit
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '@' | ':'))
        {
            return Err(format!(
                "service unit `{}` contains unsupported characters",
                self.unit
            ));
        }
        if let Some(target) = &self.target {
            if !target.is_absolute() {
                return Err("service target must be an absolute path".into());
            }
        }
        Ok(())
    }

    pub fn summary(&self) -> String {
        format!("{} {}", self.manager.as_str(), self.unit)
    }

    pub fn command_hint(&self, action: ServiceAction) -> String {
        match self.manager {
            ServiceManager::LaunchdGui => match action {
                ServiceAction::Start => match &self.target {
                    Some(target) => {
                        format!("launchctl bootstrap gui/{} {}", self.uid, target.display())
                    }
                    None => format!("launchctl kickstart gui/{}/{}", self.uid, self.unit),
                },
                ServiceAction::Stop => {
                    format!("launchctl bootout gui/{}/{}", self.uid, self.unit)
                }
                ServiceAction::Restart => {
                    format!("launchctl kickstart -k gui/{}/{}", self.uid, self.unit)
                }
            },
            ServiceManager::LaunchdSystem => match action {
                ServiceAction::Start => format!(
                    "sudo launchctl bootstrap system {}",
                    self.target
                        .as_ref()
                        .map(|target| target.display().to_string())
                        .unwrap_or_else(|| "<plist>".to_string())
                ),
                ServiceAction::Stop => {
                    format!("sudo launchctl bootout system/{}", self.unit)
                }
                ServiceAction::Restart => {
                    format!("sudo launchctl kickstart -k system/{}", self.unit)
                }
            },
            ServiceManager::SystemdUser => {
                format!("systemctl --user {} {}", action.as_str(), self.unit)
            }
            ServiceManager::SystemdSystem => {
                format!("sudo systemctl {} {}", action.as_str(), self.unit)
            }
        }
    }
}

pub fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ServiceMarker {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<ServiceIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceMarkerState {
    Managed(ServiceIdentity),
    ManagedWithoutIdentity,
}

pub fn write_marker(data_dir: &Path, identity: Option<&ServiceIdentity>) -> std::io::Result<()> {
    if let Some(identity) = identity {
        identity
            .validate()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
    }
    let marker = ServiceMarker {
        version: MARKER_VERSION,
        identity: identity.cloned(),
    };
    let bytes = serde_json::to_vec_pretty(&marker)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    fsutil::write_private_atomic(&data_dir.join(MARKER_FILE), &bytes)
}

/// Remove the service marker after the service has been removed. Returns
/// `true` when a marker existed, `false` when there was nothing to remove.
pub fn remove_marker(data_dir: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(data_dir.join(MARKER_FILE)) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

pub fn load_marker(data_dir: &Path) -> Result<Option<ServiceMarkerState>, String> {
    let path = data_dir.join(MARKER_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("read {} failed: {err}", path.display())),
    };
    let marker: ServiceMarker = serde_json::from_str(&text).map_err(|err| {
        format!(
            "parse {} failed: {err}; stop the daemon and run `dfr service forget`, or remove the file manually",
            path.display()
        )
    })?;
    if marker.version != MARKER_VERSION {
        return Err(format!(
            "unsupported service marker version {} in {}; stop the daemon and run `dfr service forget`, or remove the file manually",
            marker.version,
            path.display()
        ));
    }
    match marker.identity {
        Some(identity) => {
            identity.validate()?;
            Ok(Some(ServiceMarkerState::Managed(identity)))
        }
        None => Ok(Some(ServiceMarkerState::ManagedWithoutIdentity)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

impl ServiceAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceReport {
    Started,
    Stopped,
    Restarted,
    AlreadyRunning,
    NotRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeState {
    Missing,
    Stopped { loaded: bool },
    Running { loaded: bool, pid: Option<u32> },
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("service control is unavailable: {0}")]
    Unsupported(String),
    #[error("service is not installed: {0}")]
    Missing(String),
    #[error("cannot determine service state: {0}")]
    Probe(String),
    #[error(
        "service MainPID {service_pid:?} does not match daemon PID {daemon_pid}; refusing to {action}"
    )]
    PidMismatch {
        action: &'static str,
        daemon_pid: u32,
        service_pid: Option<u32>,
    },
    #[error("service command failed: {0}")]
    Command(String),
    #[error("invalid service identity: {0}")]
    Invalid(String),
}

pub fn control(
    identity: &ServiceIdentity,
    action: ServiceAction,
    expected_pid: Option<u32>,
) -> Result<ServiceReport, ServiceError> {
    identity.validate().map_err(ServiceError::Invalid)?;
    ensure_supported(identity, action)?;
    let probe = probe(identity)?;
    verify_expected_pid(probe, action, expected_pid)?;
    match identity.manager {
        ServiceManager::LaunchdGui => control_launchd(identity, action, probe),
        ServiceManager::SystemdUser => control_systemd(identity, action, probe),
        _ => Err(ServiceError::Unsupported(format!(
            "automatic service control is not available; run `{}`",
            identity.command_hint(action)
        ))),
    }
}

fn ensure_supported(identity: &ServiceIdentity, action: ServiceAction) -> Result<(), ServiceError> {
    if !identity.manager.is_user_scope() {
        return Err(ServiceError::Unsupported(format!(
            "system-level services require elevated privileges; run `{}`",
            identity.command_hint(action)
        )));
    }
    if !identity.manager.platform_supported() {
        return Err(ServiceError::Unsupported(format!(
            "{} is not available on this platform; run `{}`",
            identity.manager.as_str(),
            identity.command_hint(action)
        )));
    }
    if identity.uid != current_uid() {
        return Err(ServiceError::Unsupported(format!(
            "service runs as uid {} but this process is uid {}; run `dfr` as the service user or run `{}`",
            identity.uid,
            current_uid(),
            identity.command_hint(action)
        )));
    }
    Ok(())
}

fn verify_expected_pid(
    probe: ProbeState,
    action: ServiceAction,
    expected_pid: Option<u32>,
) -> Result<(), ServiceError> {
    let Some(expected) = expected_pid else {
        return Ok(());
    };
    match probe {
        ProbeState::Running { pid: Some(pid), .. } if pid == expected => Ok(()),
        ProbeState::Running { pid, .. } => Err(ServiceError::PidMismatch {
            action: action.as_str(),
            daemon_pid: expected,
            service_pid: pid,
        }),
        ProbeState::Stopped { .. } => Err(ServiceError::Probe(format!(
            "daemon reports pid {expected} but the service is not running"
        ))),
        ProbeState::Missing => Err(ServiceError::Missing(
            "service marker refers to a service that is no longer installed".into(),
        )),
    }
}

fn probe(identity: &ServiceIdentity) -> Result<ProbeState, ServiceError> {
    match identity.manager {
        ServiceManager::LaunchdGui => probe_launchd(identity),
        ServiceManager::SystemdUser => probe_systemd(identity),
        _ => Err(ServiceError::Unsupported(
            "automatic service control is not available".into(),
        )),
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_launchd_state(text: &str) -> (bool, Option<u32>) {
    let mut running = false;
    let mut pid = None;
    for line in text.lines() {
        let line = line.trim();
        if line == "state = running" {
            running = true;
        } else if pid.is_none() {
            if let Some(value) = line.strip_prefix("pid = ") {
                pid = value.trim().parse::<u32>().ok();
            }
        }
    }
    (running, pid)
}

#[cfg(target_os = "macos")]
fn probe_launchd(identity: &ServiceIdentity) -> Result<ProbeState, ServiceError> {
    let job = format!("gui/{}/{}", identity.uid, identity.unit);
    let output = Command::new("/bin/launchctl")
        .arg("print")
        .arg(&job)
        .output()
        .map_err(|err| ServiceError::Probe(format!("run launchctl print failed: {err}")))?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        let (running, pid) = parse_launchd_state(&text);
        return Ok(if running {
            ProbeState::Running { loaded: true, pid }
        } else {
            ProbeState::Stopped { loaded: true }
        });
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if stderr.contains("could not find service")
        || stderr.contains("service not found")
        || stderr.contains("no such process")
    {
        if identity
            .target
            .as_ref()
            .is_some_and(|target| target.is_file())
        {
            Ok(ProbeState::Stopped { loaded: false })
        } else {
            Ok(ProbeState::Missing)
        }
    } else {
        Err(ServiceError::Probe(format!(
            "launchctl print {job} failed: {}",
            stderr.trim()
        )))
    }
}

#[cfg(not(target_os = "macos"))]
fn probe_launchd(_identity: &ServiceIdentity) -> Result<ProbeState, ServiceError> {
    Err(ServiceError::Unsupported(
        "launchd is only available on macOS".into(),
    ))
}

#[cfg(target_os = "macos")]
fn control_launchd(
    identity: &ServiceIdentity,
    action: ServiceAction,
    probe: ProbeState,
) -> Result<ServiceReport, ServiceError> {
    match action {
        ServiceAction::Start => match probe {
            ProbeState::Running { .. } => Ok(ServiceReport::AlreadyRunning),
            ProbeState::Stopped { loaded: true } => {
                kickstart(identity, false)?;
                Ok(ServiceReport::Started)
            }
            ProbeState::Stopped { loaded: false } => {
                bootstrap(identity)?;
                Ok(ServiceReport::Started)
            }
            ProbeState::Missing => Err(ServiceError::Missing(format!(
                "launchd job {} and its plist target are missing",
                identity.unit
            ))),
        },
        ServiceAction::Stop => match probe {
            ProbeState::Running { .. } | ProbeState::Stopped { loaded: true } => {
                bootout(identity)?;
                Ok(ServiceReport::Stopped)
            }
            ProbeState::Stopped { loaded: false } | ProbeState::Missing => {
                Ok(ServiceReport::NotRunning)
            }
        },
        ServiceAction::Restart => match probe {
            ProbeState::Running { .. } => {
                kickstart(identity, true)?;
                Ok(ServiceReport::Restarted)
            }
            ProbeState::Stopped { loaded: true } => {
                kickstart(identity, false)?;
                Ok(ServiceReport::Started)
            }
            ProbeState::Stopped { loaded: false } => {
                bootstrap(identity)?;
                Ok(ServiceReport::Started)
            }
            ProbeState::Missing => Err(ServiceError::Missing(format!(
                "launchd job {} and its plist target are missing",
                identity.unit
            ))),
        },
    }
}

#[cfg(not(target_os = "macos"))]
fn control_launchd(
    identity: &ServiceIdentity,
    action: ServiceAction,
    _probe: ProbeState,
) -> Result<ServiceReport, ServiceError> {
    Err(ServiceError::Unsupported(format!(
        "launchd is only available on macOS; run `{}`",
        identity.command_hint(action)
    )))
}

#[cfg(target_os = "macos")]
fn kickstart(identity: &ServiceIdentity, restart: bool) -> Result<(), ServiceError> {
    let job = format!("gui/{}/{}", identity.uid, identity.unit);
    let mut command = Command::new("/bin/launchctl");
    command.arg("kickstart");
    if restart {
        command.arg("-k");
    }
    command.arg(job);
    run_command(command, "launchctl kickstart")
}

#[cfg(target_os = "macos")]
fn bootout(identity: &ServiceIdentity) -> Result<(), ServiceError> {
    let job = format!("gui/{}/{}", identity.uid, identity.unit);
    run_command(
        {
            let mut command = Command::new("/bin/launchctl");
            command.arg("bootout").arg(&job);
            command
        },
        "launchctl bootout",
    )
}

#[cfg(target_os = "macos")]
fn bootstrap(identity: &ServiceIdentity) -> Result<(), ServiceError> {
    let target = identity.target.as_ref().ok_or_else(|| {
        ServiceError::Missing(format!(
            "launchd target is not configured for {}; set DFRELAY_SERVICE_TARGET",
            identity.unit
        ))
    })?;
    if !target.is_file() {
        return Err(ServiceError::Missing(format!(
            "launchd target {} is missing",
            target.display()
        )));
    }
    let domain = format!("gui/{}", identity.uid);
    run_command(
        {
            let mut command = Command::new("/bin/launchctl");
            command.arg("bootstrap").arg(domain).arg(target);
            command
        },
        "launchctl bootstrap",
    )
}

#[cfg(any(target_os = "linux", test))]
fn parse_systemd_show(text: &str) -> (Option<String>, Option<String>, Option<u32>) {
    let mut load_state = None;
    let mut active_state = None;
    let mut main_pid = None;
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        match key {
            "LoadState" => load_state = Some(value.trim().to_string()),
            "ActiveState" => active_state = Some(value.trim().to_string()),
            "MainPID" => main_pid = value.trim().parse::<u32>().ok(),
            _ => {}
        }
    }
    (load_state, active_state, main_pid)
}

#[cfg(target_os = "linux")]
fn systemctl_path() -> Result<PathBuf, ServiceError> {
    for path in ["/usr/bin/systemctl", "/bin/systemctl"] {
        if Path::new(path).is_file() {
            return Ok(PathBuf::from(path));
        }
    }
    Err(ServiceError::Command(
        "systemctl was not found in /usr/bin or /bin".into(),
    ))
}

#[cfg(target_os = "linux")]
fn probe_systemd(identity: &ServiceIdentity) -> Result<ProbeState, ServiceError> {
    let systemctl = systemctl_path()?;
    let output = Command::new(systemctl)
        .args([
            "--user",
            "show",
            "--no-pager",
            "--property=LoadState",
            "--property=ActiveState",
            "--property=MainPID",
        ])
        .arg(&identity.unit)
        .output()
        .map_err(|err| ServiceError::Probe(format!("run systemctl show failed: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ServiceError::Probe(format!(
            "systemctl --user show {} failed: {}",
            identity.unit,
            stderr.trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (load_state, active_state, main_pid) = parse_systemd_show(&text);
    if load_state.as_deref() == Some("not-found") {
        return Ok(ProbeState::Missing);
    }
    let active = matches!(
        active_state.as_deref(),
        Some("active" | "activating" | "reloading")
    );
    if active {
        Ok(ProbeState::Running {
            loaded: true,
            pid: main_pid.filter(|pid| *pid > 0),
        })
    } else {
        Ok(ProbeState::Stopped { loaded: true })
    }
}

#[cfg(not(target_os = "linux"))]
fn probe_systemd(_identity: &ServiceIdentity) -> Result<ProbeState, ServiceError> {
    Err(ServiceError::Unsupported(
        "systemd is only available on Linux".into(),
    ))
}

#[cfg(target_os = "linux")]
fn control_systemd(
    identity: &ServiceIdentity,
    action: ServiceAction,
    probe: ProbeState,
) -> Result<ServiceReport, ServiceError> {
    match action {
        ServiceAction::Start => match probe {
            ProbeState::Running { .. } => Ok(ServiceReport::AlreadyRunning),
            ProbeState::Stopped { .. } => {
                run_systemctl(identity, "start")?;
                Ok(ServiceReport::Started)
            }
            ProbeState::Missing => Err(ServiceError::Missing(format!(
                "systemd unit {} is not installed",
                identity.unit
            ))),
        },
        ServiceAction::Stop => match probe {
            ProbeState::Running { .. } => {
                run_systemctl(identity, "stop")?;
                Ok(ServiceReport::Stopped)
            }
            ProbeState::Stopped { .. } | ProbeState::Missing => Ok(ServiceReport::NotRunning),
        },
        ServiceAction::Restart => match probe {
            ProbeState::Running { .. } => {
                run_systemctl(identity, "restart")?;
                Ok(ServiceReport::Restarted)
            }
            ProbeState::Stopped { .. } => {
                run_systemctl(identity, "restart")?;
                Ok(ServiceReport::Started)
            }
            ProbeState::Missing => Err(ServiceError::Missing(format!(
                "systemd unit {} is not installed",
                identity.unit
            ))),
        },
    }
}

#[cfg(not(target_os = "linux"))]
fn control_systemd(
    identity: &ServiceIdentity,
    action: ServiceAction,
    _probe: ProbeState,
) -> Result<ServiceReport, ServiceError> {
    Err(ServiceError::Unsupported(format!(
        "systemd is only available on Linux; run `{}`",
        identity.command_hint(action)
    )))
}

#[cfg(target_os = "linux")]
fn run_systemctl(identity: &ServiceIdentity, verb: &str) -> Result<(), ServiceError> {
    let systemctl = systemctl_path()?;
    let mut command = Command::new(systemctl);
    command.args(["--user", verb]).arg(&identity.unit);
    run_command(command, &format!("systemctl --user {verb}"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_command(mut command: Command, label: &str) -> Result<(), ServiceError> {
    let output = command
        .output()
        .map_err(|err| ServiceError::Command(format!("{label} failed to start: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if !stderr.trim().is_empty() {
        stderr.trim()
    } else if !stdout.trim().is_empty() {
        stdout.trim()
    } else {
        "no output"
    };
    Err(ServiceError::Command(format!(
        "{label} exited with {}: {detail}",
        output.status
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn identity(manager: ServiceManager) -> ServiceIdentity {
        ServiceIdentity {
            manager,
            unit: "dfrelay.service".into(),
            target: None,
            uid: current_uid(),
        }
    }

    #[test]
    fn manager_roundtrip() {
        for (value, manager) in [
            ("launchd-gui", ServiceManager::LaunchdGui),
            ("launchd-system", ServiceManager::LaunchdSystem),
            ("systemd-user", ServiceManager::SystemdUser),
            ("systemd-system", ServiceManager::SystemdSystem),
        ] {
            assert_eq!(ServiceManager::parse(value), Some(manager));
            assert_eq!(manager.as_str(), value);
        }
        assert_eq!(ServiceManager::parse("launchd"), None);
    }

    #[test]
    fn validates_units_and_targets() {
        let mut identity = identity(ServiceManager::SystemdUser);
        assert!(identity.validate().is_ok());
        identity.unit = "--help".into();
        assert!(identity.validate().is_err());
        identity.unit = "dfrelay.service; rm".into();
        assert!(identity.validate().is_err());
        identity.unit = "dfrelay.service".into();
        identity.target = Some(PathBuf::from("relative.plist"));
        assert!(identity.validate().is_err());
        identity.target = Some(PathBuf::from("/tmp/dfrelay.plist"));
        assert!(identity.validate().is_ok());
    }

    #[test]
    fn marker_roundtrip_is_private() {
        let dir = TempDir::new().unwrap();
        let mut identity = identity(ServiceManager::LaunchdGui);
        identity.target = Some(PathBuf::from(
            "/Users/test/Library/LaunchAgents/dfrelay.plist",
        ));
        write_marker(dir.path(), Some(&identity)).unwrap();
        let path = dir.path().join(MARKER_FILE);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            load_marker(dir.path()).unwrap(),
            Some(ServiceMarkerState::Managed(identity))
        );
    }

    #[test]
    fn marker_without_identity_still_blocks_ad_hoc_fallback() {
        let dir = TempDir::new().unwrap();
        write_marker(dir.path(), None).unwrap();
        assert_eq!(
            load_marker(dir.path()).unwrap(),
            Some(ServiceMarkerState::ManagedWithoutIdentity)
        );
    }

    #[test]
    fn missing_marker_is_none() {
        let dir = TempDir::new().unwrap();
        assert_eq!(load_marker(dir.path()).unwrap(), None);
    }

    #[test]
    fn marker_can_be_removed_for_ad_hoc_recovery() {
        let dir = TempDir::new().unwrap();
        write_marker(dir.path(), None).unwrap();
        assert!(remove_marker(dir.path()).unwrap());
        assert_eq!(load_marker(dir.path()).unwrap(), None);
        assert!(!remove_marker(dir.path()).unwrap());
    }

    #[test]
    fn launchd_state_parsing() {
        let (running, pid) = parse_launchd_state(
            "\tgui/501/dfrelay = {\n\t\tstate = running\n\t\tpid = 8273\n\t\tpid = 9999\n\t}\n",
        );
        assert!(running);
        assert_eq!(pid, Some(8273));

        let (running, pid) = parse_launchd_state("\tstate = not running\n\tpid = 0\n");
        assert!(!running);
        assert_eq!(pid, Some(0));
    }

    #[test]
    fn systemd_state_parsing() {
        let (load, active, pid) =
            parse_systemd_show("LoadState=loaded\nActiveState=active\nMainPID=8273\n");
        assert_eq!(load.as_deref(), Some("loaded"));
        assert_eq!(active.as_deref(), Some("active"));
        assert_eq!(pid, Some(8273));

        let (load, active, _) =
            parse_systemd_show("LoadState=not-found\nActiveState=inactive\nMainPID=0\n");
        assert_eq!(load.as_deref(), Some("not-found"));
        assert_eq!(active.as_deref(), Some("inactive"));
    }

    #[test]
    fn launchd_hints_include_target() {
        let mut identity = identity(ServiceManager::LaunchdGui);
        identity.uid = 501;
        identity.unit = "io.github.fynhq.dfrelay".into();
        identity.target = Some(PathBuf::from(
            "/Users/example/Library/LaunchAgents/io.github.fynhq.dfrelay.plist",
        ));
        assert_eq!(
            identity.command_hint(ServiceAction::Start),
            "launchctl bootstrap gui/501 /Users/example/Library/LaunchAgents/io.github.fynhq.dfrelay.plist"
        );
        assert_eq!(
            identity.command_hint(ServiceAction::Restart),
            "launchctl kickstart -k gui/501/io.github.fynhq.dfrelay"
        );
    }

    #[test]
    fn expected_pid_must_match() {
        assert!(verify_expected_pid(
            ProbeState::Running {
                loaded: true,
                pid: Some(42)
            },
            ServiceAction::Stop,
            Some(42)
        )
        .is_ok());
        assert!(matches!(
            verify_expected_pid(
                ProbeState::Running {
                    loaded: true,
                    pid: Some(41)
                },
                ServiceAction::Stop,
                Some(42)
            ),
            Err(ServiceError::PidMismatch { .. })
        ));
        assert!(matches!(
            verify_expected_pid(
                ProbeState::Stopped { loaded: true },
                ServiceAction::Restart,
                Some(42)
            ),
            Err(ServiceError::Probe(_))
        ));
    }
}
