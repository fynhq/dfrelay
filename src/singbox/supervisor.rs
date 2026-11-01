use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
use std::process::Stdio;
use std::time::Duration;

use rand::RngCore;
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
    #[cfg(unix)]
    unsafe {
        libc::kill(proxy.pid as i32, libc::SIGTERM);
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
    for _ in 0..20 {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(SupervisorError::Health(format!(
                "exited early with {status}"
            )));
        }
        let attempt = tokio::time::timeout(
            Duration::from_millis(300),
            tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
        )
        .await;
        if matches!(attempt, Ok(Ok(_))) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
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
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
}
