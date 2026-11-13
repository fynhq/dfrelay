use std::io::Read;
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::config::{Config, Paths};
use crate::fsutil;
use crate::logging::{LogLevel, Logger};
use crate::subscription::model::Node;

use super::binary::BinaryManager;
use super::config_gen::{self, ConfigGenError};

#[derive(Debug, Clone)]
pub struct ClashApiInfo {
    pub port: u16,
    pub secret: String,
}

pub struct RunningProxy {
    pub child: Child,
    pub pid: u32,
    pub clash: ClashApiInfo,
    pub listen_v4: String,
    pub port: u16,
    pub ipv6: bool,
    pub lan_auth: Option<(String, String)>,
    log_tasks: Vec<JoinHandle<()>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("no nodes available")]
    NoNodes,
    #[error("sing-box binary not found")]
    BinaryMissing,
    #[error("port {port} is not available: {reason}")]
    Port { port: u16, reason: String },
    #[error("config generation failed: {0}")]
    Config(#[from] ConfigGenError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sing-box check failed: {0}")]
    Check(String),
    #[error("sing-box failed to become healthy: {0}")]
    Health(String),
}

/// Validate a proposed configuration without touching the running instance.
///
/// The check runs against a temporary file, so a failed proposal never replaces
/// `runtime/config.json` and never stops the current sing-box process. Listener
/// port conflicts cannot be validated while the old instance still owns the
/// port; such failures are reported by `start` after the caller stops.
pub fn preflight(
    paths: &Paths,
    config: &Config,
    nodes: &[Node],
    preferred: Option<&str>,
) -> Result<(), SupervisorError> {
    if nodes.is_empty() {
        return Err(SupervisorError::NoNodes);
    }
    let binary = BinaryManager::new(paths.clone())
        .locate()
        .ok_or(SupervisorError::BinaryMissing)?;
    let clash_port = allocate_clash_port(config.port)?;
    let secret = random_secret();
    let generated = config_gen::generate_with_selection(
        config,
        nodes,
        config.enable_ipv6,
        clash_port,
        &secret,
        &paths.cache_file(),
        preferred,
    )?;
    fsutil::ensure_private_dir(&paths.runtime_dir)?;
    let candidate = paths.runtime_config().with_extension("json.preflight");
    let generated = serde_json::to_string_pretty(&generated)?;
    fsutil::write_private_file(&candidate, generated.as_bytes())?;
    let result = run_check_blocking(&binary, &candidate);
    let _ = std::fs::remove_file(&candidate);
    result
}

fn run_check_blocking(binary: &Path, config: &Path) -> Result<(), SupervisorError> {
    let mut child = StdCommand::new(binary)
        .args(["check", "-c"])
        .arg(config)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if status.success() {
                return Ok(());
            }
            return Err(SupervisorError::Check(stderr.trim().to_string()));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SupervisorError::Check("timeout".to_string()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub async fn start(
    paths: &Paths,
    config: &Config,
    nodes: &[Node],
    logger: &Logger,
    preferred: Option<&str>,
) -> Result<RunningProxy, SupervisorError> {
    if nodes.is_empty() {
        return Err(SupervisorError::NoNodes);
    }
    let binary = BinaryManager::new(paths.clone())
        .locate()
        .ok_or(SupervisorError::BinaryMissing)?;

    let port = config.port;
    let listen_v4 = if config.allow_lan {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    };
    TcpListener::bind((listen_v4, port)).map_err(|err| SupervisorError::Port {
        port,
        reason: err.to_string(),
    })?;
    let ipv6_available =
        config.enable_ipv6 && TcpListener::bind((Ipv6Addr::LOCALHOST, port)).is_ok();
    if config.enable_ipv6 && !ipv6_available {
        logger.app(
            LogLevel::Warn,
            format!("IPv6 loopback :{port} unavailable; starting IPv4-only"),
        );
    }

    let clash_port = allocate_clash_port(port)?;
    let secret = random_secret();
    let generated = config_gen::generate_with_selection(
        config,
        nodes,
        ipv6_available,
        clash_port,
        &secret,
        &paths.cache_file(),
        preferred,
    )?;
    fsutil::ensure_private_dir(&paths.runtime_dir)?;
    let runtime_config = paths.runtime_config();
    let candidate = runtime_config.with_extension("json.candidate");
    let generated = serde_json::to_string_pretty(&generated)?;
    fsutil::write_private_file(&candidate, generated.as_bytes())?;

    let check = Command::new(&binary)
        .args(["check", "-c"])
        .arg(&candidate)
        .kill_on_drop(true)
        .output();
    let check = match tokio::time::timeout(Duration::from_secs(15), check).await {
        Err(_) => {
            let _ = std::fs::remove_file(&candidate);
            return Err(SupervisorError::Check("timeout".to_string()));
        }
        Ok(Err(err)) => {
            let _ = std::fs::remove_file(&candidate);
            return Err(SupervisorError::Io(err));
        }
        Ok(Ok(output)) => output,
    };
    if !check.status.success() {
        let _ = std::fs::remove_file(&candidate);
        return Err(SupervisorError::Check(
            String::from_utf8_lossy(&check.stderr).trim().to_string(),
        ));
    }
    if let Err(err) = std::fs::rename(&candidate, &runtime_config) {
        let _ = std::fs::remove_file(&candidate);
        return Err(SupervisorError::Io(err));
    }

    if config.allow_lan {
        logger.app(
            LogLevel::Warn,
            format!("LAN access enabled: mixed proxy listens on 0.0.0.0:{port}"),
        );
    }
    logger.app(
        LogLevel::Info,
        format!(
            "starting sing-box on {}:{} (clash api 127.0.0.1:{})",
            listen_v4, port, clash_port
        ),
    );
    let mut child = Command::new(&binary)
        .args(["run", "-c"])
        .arg(&runtime_config)
        .arg("-D")
        .arg(&paths.data_dir)
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id().unwrap_or(0);
    let mut log_tasks = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        log_tasks.push(spawn_log_reader(stdout, logger.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        log_tasks.push(spawn_log_reader(stderr, logger.clone()));
    }

    if let Err(err) = wait_healthy(port, &mut child).await {
        let _ = child.kill().await;
        for task in log_tasks {
            task.abort();
        }
        logger.singbox(LogLevel::Error, format!("startup failed: {err}"));
        return Err(err);
    }

    logger.app(LogLevel::Info, format!("sing-box started (pid {pid})"));
    Ok(RunningProxy {
        child,
        pid,
        clash: ClashApiInfo {
            port: clash_port,
            secret,
        },
        listen_v4: listen_v4.to_string(),
        port,
        ipv6: ipv6_available,
        lan_auth: config
            .lan_auth()
            .map(|(username, password)| (username.to_string(), password.to_string())),
        log_tasks,
    })
}

pub async fn stop(mut proxy: RunningProxy, logger: &Logger) {
    // Avoid signalling a pid that has already been reaped; this also keeps the
    // cached pid from racing with a reused process id.
    let already_exited = matches!(proxy.child.try_wait(), Ok(Some(_)));
    #[cfg(unix)]
    if !already_exited && proxy.pid != 0 {
        unsafe {
            libc::kill(proxy.pid as i32, libc::SIGTERM);
        }
    }
    let waited = tokio::time::timeout(Duration::from_secs(5), proxy.child.wait()).await;
    if waited.is_err() {
        let _ = proxy.child.kill().await;
    }
    for task in proxy.log_tasks {
        task.abort();
    }
    logger.app(LogLevel::Info, "sing-box stopped");
}

fn spawn_log_reader<R>(reader: R, logger: Logger) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let (level, message) = crate::logging::parse_singbox_line(&line);
            if message.is_empty() {
                continue;
            }
            logger.singbox(level.unwrap_or(LogLevel::Info), message);
        }
    })
}

async fn wait_healthy(port: u16, child: &mut Child) -> Result<(), SupervisorError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(SupervisorError::Health(format!(
                "exited early with {status}"
            )));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let attempt = tokio::time::timeout(
            remaining.min(Duration::from_millis(300)),
            tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
        )
        .await;
        if matches!(attempt, Ok(Ok(_))) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(remaining.min(Duration::from_millis(250))).await;
    }
    Err(SupervisorError::Health(
        "mixed port did not become reachable".to_string(),
    ))
}

fn allocate_clash_port(proxy_port: u16) -> Result<u16, SupervisorError> {
    for _ in 0..10 {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|err| SupervisorError::Port {
                port: 0,
                reason: err.to_string(),
            })?;
        let port = listener
            .local_addr()
            .map_err(|err| SupervisorError::Port {
                port: 0,
                reason: err.to_string(),
            })?
            .port();
        if port != proxy_port {
            return Ok(port);
        }
    }
    Err(SupervisorError::Port {
        port: 0,
        reason: "could not allocate clash api port".to_string(),
    })
}

fn random_secret() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    crate::hex::encode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_32_hex_chars() {
        let secret = random_secret();
        assert_eq!(secret.len(), 32);
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn clash_port_is_allocated() {
        let port = allocate_clash_port(12000).unwrap();
        assert!(port > 0);
        assert_ne!(port, 12000);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_checks_the_generated_config() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::TempDir;

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
        let binary = paths.binary();
        fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

        let node = Node::new(
            "node".to_string(),
            "test".to_string(),
            "shadowsocks".to_string(),
            "example.com".to_string(),
            443,
            serde_json::json!({
                "type": "shadowsocks",
                "server": "example.com",
                "server_port": 443,
                "method": "aes-128-gcm",
                "password": "secret"
            }),
        );
        let config = Config::default();
        assert!(preflight(&paths, &config, std::slice::from_ref(&node), None).is_ok());

        fs::write(&binary, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            preflight(&paths, &config, &[node], None),
            Err(SupervisorError::Check(_))
        ));
    }

    #[cfg(unix)]
    fn test_paths(dir: &Path) -> Paths {
        Paths {
            config_file: dir.join("config.toml"),
            data_dir: dir.to_path_buf(),
            bin_dir: dir.join("bin"),
            cache_dir: dir.join("cache"),
            runtime_dir: dir.join("runtime"),
            logs_dir: dir.join("logs"),
            inbox_dir: dir.join("inbox"),
        }
    }

    #[cfg(unix)]
    fn test_node() -> Node {
        Node::new(
            "node".to_string(),
            "test".to_string(),
            "shadowsocks".to_string(),
            "example.com".to_string(),
            443,
            serde_json::json!({
                "type": "shadowsocks",
                "server": "example.com",
                "server_port": 443,
                "method": "aes-128-gcm",
                "password": "secret"
            }),
        )
    }

    #[cfg(unix)]
    fn free_port() -> u16 {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// A fake sing-box that answers `version`/`check` and serves the mixed
    /// listener with a Python socket so `wait_healthy` can succeed.
    #[cfg(unix)]
    fn fake_server_script() -> &'static str {
        r#"#!/bin/sh
if [ "$1" = version ]; then echo "sing-box version 1.14.4"; exit 0; fi
if [ "$1" = check ]; then exit 0; fi
exec python3 -c 'import json,socket,sys,time
c=json.load(open(sys.argv[1]))
p=[i["listen_port"] for i in c["inbounds"] if i["type"]=="mixed"][0]
s=socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", p))
s.listen(16)
time.sleep(30)' "$3"
"#
    }

    #[cfg(unix)]
    fn install_fake_binary(paths: &Paths, script: &str) {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        fs::write(paths.binary(), script).unwrap();
        fs::set_permissions(paths.binary(), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn starts_and_stops_a_fake_sing_box() {
        let has_python = StdCommand::new("python3")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !has_python {
            eprintln!("skipping fake sing-box lifecycle test: python3 is unavailable");
            return;
        }

        let dir = tempfile::TempDir::new().unwrap();
        let paths = test_paths(dir.path());
        paths.ensure().unwrap();
        install_fake_binary(&paths, fake_server_script());
        let port = free_port();
        let config = Config {
            port,
            enable_ipv6: false,
            ..Config::default()
        };
        let logger = Logger::new(&paths.logs_dir, LogLevel::Info, LogLevel::Info).unwrap();

        let proxy = start(&paths, &config, &[test_node()], &logger, None)
            .await
            .unwrap();
        assert_eq!(proxy.port, port);
        assert_eq!(proxy.listen_v4, "127.0.0.1");
        assert!(!proxy.ipv6);
        assert!(proxy.pid > 0);
        assert!(paths.runtime_config().is_file());

        stop(proxy, &logger).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn occupied_proxy_port_is_reported_before_spawn() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = test_paths(dir.path());
        paths.ensure().unwrap();
        install_fake_binary(&paths, "#!/bin/sh\nexit 0\n");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Config {
            port,
            enable_ipv6: false,
            ..Config::default()
        };

        let logger = Logger::new(&paths.logs_dir, LogLevel::Info, LogLevel::Info).unwrap();
        let result = start(&paths, &config, &[test_node()], &logger, None).await;

        assert!(matches!(
            result,
            Err(SupervisorError::Port { port: reported, .. }) if reported == port
        ));
        drop(listener);
    }
}
