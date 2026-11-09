mod app;
mod archive;
mod client;
mod config;
mod fsutil;
mod guard;
mod http_body;
mod i18n;
mod ipc;
mod logging;
mod netinfo;
mod selection;
mod service;
mod singbox;
mod state;
mod subscription;
mod text;
mod theme;
mod ui;
mod update;
mod version;
mod view;

use std::io::{stdout, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::TimeZone as _;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use crossterm::cursor::Show;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};

use app::App;
use config::{Config, Paths};
use i18n::Lang;
use logging::{LogLevel, Logger};
use service::{ServiceAction, ServiceError, ServiceIdentity, ServiceMarkerState, ServiceReport};
use state::ProxyStatus;
use view::DaemonStatus;

#[derive(Debug, Parser)]
#[command(
    name = "dfr",
    version = version::DISPLAY,
    about = "DFRelay terminal proxy manager",
    after_help = "Project: https://github.com/fynhq/dfrelay"
)]
struct Cli {
    /// Override the config file path.
    #[arg(short = 'c', long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Override the data directory (selects a separate daemon instance).
    #[arg(short = 'd', long = "data", global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand, Clone)]
enum Command {
    /// Start the daemon and proxy in the background.
    Start,
    /// Stop the background daemon and proxy.
    Stop,
    /// Restart the daemon and proxy in the background.
    Restart,
    /// Show daemon and proxy status.
    Status,
    /// Run the daemon in the foreground for systemd/launchd.
    Serve,
    /// Manage the service marker left by a service-managed daemon.
    Service {
        #[command(subcommand)]
        action: ServiceCommand,
    },
    /// Check, apply, or roll back a DFRelay update.
    Update {
        #[command(subcommand)]
        action: UpdateCommand,
    },
}

#[derive(Debug, Subcommand, Clone)]
enum UpdateCommand {
    /// Check the latest release without downloading or installing it.
    Check,
    /// Download, verify, install the latest release, and restart the daemon.
    Apply,
    /// Restore the previous binary and restart the daemon.
    Rollback,
    /// Show the update policy and the last update result.
    Status,
    /// Internal helper used by the daemon; not for manual use.
    #[command(hide = true)]
    Helper {
        #[arg(long)]
        staged: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        version: String,
    },
}

#[derive(Debug, Subcommand, Clone, Copy)]
enum ServiceCommand {
    /// Remove the service marker so `dfr start` can use an ad-hoc daemon.
    Forget,
}

/// Commands that run after configuration loading. `Service` must be handled
/// before `Config`/`Logger` are created, so it has no variant here.
#[derive(Debug, Clone)]
enum RuntimeCommand {
    Start,
    Stop,
    Restart,
    Status,
    Serve,
    Update(UpdateCommand),
}

impl Command {
    fn into_runtime(self) -> Option<RuntimeCommand> {
        match self {
            Command::Start => Some(RuntimeCommand::Start),
            Command::Stop => Some(RuntimeCommand::Stop),
            Command::Restart => Some(RuntimeCommand::Restart),
            Command::Status => Some(RuntimeCommand::Status),
            Command::Serve => Some(RuntimeCommand::Serve),
            Command::Update { action } => Some(RuntimeCommand::Update(action)),
            Command::Service { .. } => None,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let paths = Paths::discover(cli.config.clone(), cli.data_dir.clone())?;
    paths.ensure()?;
    if let Some(Command::Service { action }) = &cli.command {
        return service_command(&paths, *action).await;
    }
    let (config, warnings) = Config::load_or_default(&paths.config_file);
    let logger = Logger::new(
        &paths.logs_dir,
        config.app_log_level,
        config.singbox_log_level,
    )?;
    for warning in warnings {
        logger.app(LogLevel::Warn, warning);
    }

    let command = cli.command.clone().and_then(Command::into_runtime);
    match command {
        Some(RuntimeCommand::Start) => return start_daemon(&cli, &paths).await,
        Some(RuntimeCommand::Stop) => return stop_daemon(&paths).await,
        Some(RuntimeCommand::Restart) => return restart_daemon(&cli, &paths).await,
        Some(RuntimeCommand::Status) => return print_status(&paths).await,
        Some(RuntimeCommand::Serve) => return serve(config, paths, logger).await,
        Some(RuntimeCommand::Update(action)) => {
            return update_command(&paths, &config, &logger, action).await;
        }
        None => {}
    }

    if !std::io::stdin().is_terminal() || !stdout().is_terminal() {
        bail!("DFRelay requires an interactive terminal");
    }
    let socket = paths.control_socket();
    if !ipc::daemon_is_running(&socket).await {
        bail!("DFRelay daemon is not running; run `dfr start` or start the system service");
    }
    install_panic_hook();
    logger.app(LogLevel::Info, "TUI client attached");
    client::run(socket, config.language).await
}

async fn serve(config: Config, paths: Paths, logger: Logger) -> anyhow::Result<()> {
    let managed = std::env::var("DFRELAY_SERVICE").as_deref() == Ok("1");
    let service = match ServiceIdentity::from_env() {
        Ok(identity) => identity,
        Err(err) => {
            logger.app(LogLevel::Warn, format!("service identity ignored: {err}"));
            None
        }
    };
    if managed && service.is_none() {
        logger.app(
            LogLevel::Warn,
            "service-managed without a service identity; automatic start/stop/restart is unavailable",
        );
    }
    logger.app(
        LogLevel::Info,
        format!(
            "DFRelay v{} daemon starting{}",
            version::DISPLAY,
            if managed { " (service-managed)" } else { "" }
        ),
    );
    let _daemon_lock = match ipc::acquire_daemon_lock(&paths.data_dir) {
        Ok(lock) => lock,
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            logger.app(LogLevel::Warn, "daemon already running");
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    let socket = paths.control_socket();
    let listener = match ipc::bind_listener(&socket).await {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            logger.app(LogLevel::Warn, "daemon already running");
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    if managed {
        if let Err(err) = service::write_marker(&paths.data_dir, service.as_ref()) {
            logger.app(
                LogLevel::Warn,
                format!("write service marker failed: {err}"),
            );
        }
    }
    let mut app = App::new(config, paths, logger).await;
    app.run_daemon(listener, socket, managed, service).await?;
    Ok(())
}

async fn start_daemon(cli: &Cli, paths: &Paths) -> anyhow::Result<()> {
    let socket = paths.control_socket();
    if ipc::daemon_is_running(&socket).await {
        if let Ok(status) = ipc::control_status(&socket).await {
            if status.proxy != ProxyStatus::Running {
                let ack = ipc::control_start(&socket)
                    .await
                    .context("request proxy start")?;
                if !ack.ok {
                    bail!("{}", ack.message);
                }
                println!("{}", ack.message);
            } else {
                println!("DFRelay is already running.");
            }
            return Ok(());
        }
        println!("DFRelay is already running.");
        return Ok(());
    }

    if let Some(marker) = service_marker(paths)? {
        let identity = service_identity(marker)?;
        match service::control(&identity, ServiceAction::Start, None) {
            Ok(_) => {
                wait_for_socket(&socket, Duration::from_secs(20))
                    .await
                    .with_context(|| {
                        format!(
                            "DFRelay service {} did not expose its control socket; try `dfr restart`",
                            identity.summary()
                        )
                    })?;
                println!(
                    "DFRelay started via {}; run `dfr` to attach.",
                    identity.summary()
                );
                report_start(&socket).await;
                return Ok(());
            }
            Err(err) => return Err(err).context("start DFRelay through the service manager"),
        }
    }

    ipc::spawn_daemon(cli.config.clone(), cli.data_dir.clone()).context("spawn DFRelay daemon")?;
    wait_for_socket(&socket, Duration::from_secs(8)).await?;
    println!("DFRelay started; run `dfr` to attach.");
    report_start(&socket).await;
    Ok(())
}

async fn stop_daemon(paths: &Paths) -> anyhow::Result<()> {
    let socket = paths.control_socket();
    if !ipc::daemon_is_running(&socket).await {
        if let Some(marker) = service_marker(paths)? {
            let identity = service_identity(marker)?;
            match service::control(&identity, ServiceAction::Stop, None) {
                Ok(report) => {
                    if matches!(report, ServiceReport::NotRunning) {
                        println!("DFRelay is not running.");
                    } else {
                        println!("DFRelay stopped via {}.", identity.summary());
                    }
                }
                Err(ServiceError::Missing(_)) => {
                    println!("DFRelay is not running.");
                }
                Err(err) => return Err(err).context("stop DFRelay through the service manager"),
            }
        } else {
            println!("DFRelay is not running.");
        }
        return Ok(());
    }

    let status = ipc::control_status(&socket)
        .await
        .context("query DFRelay daemon status; refusing to stop an unverifiable daemon")?;
    if status.managed {
        let Some(identity) = status.service.clone() else {
            bail!(
                "DFRelay is service-managed but has no service identity; stop it with the service manager"
            );
        };
        service::control(&identity, ServiceAction::Stop, Some(status.pid))
            .context("stop DFRelay through the service manager")?;
        wait_for_socket_gone(&socket, Duration::from_secs(15))
            .await
            .with_context(|| {
                format!(
                    "DFRelay service {} did not stop; check the service manager",
                    identity.summary()
                )
            })?;
        println!("DFRelay stopped via {}.", identity.summary());
        return Ok(());
    }

    ipc::control_shutdown(&socket).await?;
    println!("DFRelay stopped.");
    Ok(())
}

async fn restart_daemon(cli: &Cli, paths: &Paths) -> anyhow::Result<()> {
    let socket = paths.control_socket();
    let was_running = ipc::daemon_is_running(&socket).await;
    if was_running {
        let status = ipc::control_status(&socket)
            .await
            .context("query DFRelay daemon status; refusing to restart an unverifiable daemon")?;
        if status.managed {
            let Some(identity) = status.service.clone() else {
                bail!(
                    "DFRelay is service-managed but has no service identity; restart it with the service manager"
                );
            };
            let old_pid = status.pid;
            service::control(&identity, ServiceAction::Restart, Some(old_pid))
                .context("restart DFRelay through the service manager")?;
            wait_for_new_daemon(&socket, old_pid, Duration::from_secs(20))
                .await
                .with_context(|| {
                    format!(
                        "DFRelay service {} did not come back with a new daemon; check the service manager",
                        identity.summary()
                    )
                })?;
            println!(
                "DFRelay restarted via {}; run `dfr` to attach.",
                identity.summary()
            );
            report_start(&socket).await;
            return Ok(());
        }
        ipc::control_shutdown(&socket).await?;
        wait_for_daemon_lock(&paths.data_dir, Duration::from_secs(5))
            .await
            .context("wait for the previous DFRelay daemon to exit")?;
        ipc::spawn_daemon(cli.config.clone(), cli.data_dir.clone())
            .context("spawn DFRelay daemon")?;
        wait_for_socket(&socket, Duration::from_secs(8)).await?;
        println!("DFRelay restarted; run `dfr` to attach.");
        report_start(&socket).await;
        return Ok(());
    }

    if let Some(marker) = service_marker(paths)? {
        let identity = service_identity(marker)?;
        match service::control(&identity, ServiceAction::Restart, None) {
            Ok(_) => {
                wait_for_socket(&socket, Duration::from_secs(20))
                    .await
                    .with_context(|| {
                        format!(
                            "DFRelay service {} did not start; check the service manager",
                            identity.summary()
                        )
                    })?;
                println!(
                    "DFRelay started via {}; run `dfr` to attach.",
                    identity.summary()
                );
                report_start(&socket).await;
                return Ok(());
            }
            Err(err) => return Err(err).context("restart DFRelay through the service manager"),
        }
    }

    ipc::spawn_daemon(cli.config.clone(), cli.data_dir.clone()).context("spawn DFRelay daemon")?;
    wait_for_socket(&socket, Duration::from_secs(8)).await?;
    println!("DFRelay started; run `dfr` to attach.");
    report_start(&socket).await;
    Ok(())
}

async fn service_command(paths: &Paths, action: ServiceCommand) -> anyhow::Result<()> {
    match action {
        ServiceCommand::Forget => {
            if ipc::daemon_is_running(&paths.control_socket()).await {
                bail!("stop the DFRelay daemon before forgetting the service marker");
            }
            if service::remove_marker(&paths.data_dir)? {
                println!("Removed the DFRelay service marker.");
            } else {
                println!("No DFRelay service marker was found.");
            }
            Ok(())
        }
    }
}

fn service_marker(paths: &Paths) -> anyhow::Result<Option<ServiceMarkerState>> {
    service::load_marker(&paths.data_dir).map_err(anyhow::Error::msg)
}

fn service_identity(marker: ServiceMarkerState) -> anyhow::Result<service::ServiceIdentity> {
    match marker {
        ServiceMarkerState::Managed(identity) => Ok(identity),
        ServiceMarkerState::ManagedWithoutIdentity => bail!(
            "DFRelay is service-managed but its service identity is not configured; add DFRELAY_SERVICE_MANAGER/UNIT (and DFRELAY_SERVICE_TARGET for launchd) and restart the service"
        ),
    }
}

async fn update_command(
    paths: &Paths,
    config: &Config,
    logger: &Logger,
    action: UpdateCommand,
) -> anyhow::Result<()> {
    match action {
        UpdateCommand::Check => {
            println!("Current: {}", version::DISPLAY);
            if !update::is_release_build() {
                println!("Note: local builds do not update automatically.");
            }
            match update::check_for_update(config).await {
                Ok(Some(release)) => {
                    record_check(paths, logger, Some(&release.version));
                    println!("Latest:  v{}", release.version);
                    println!("Update available.");
                }
                Ok(None) => {
                    record_check(paths, logger, None);
                    println!("Latest:  no newer release");
                }
                Err(err) => bail!("update check failed: {err}"),
            }
        }
        UpdateCommand::Apply => {
            let release = update::check_for_update(config)
                .await
                .context("check for a DFRelay update")?
                .ok_or_else(|| anyhow::anyhow!("DFRelay is already up to date"))?;
            let target = update::target_binary().context("resolve the installed dfr binary")?;
            let staged = update::download_and_stage(config, paths, &release, logger)
                .await
                .context("download the DFRelay update")?;
            println!("Downloaded v{}; applying...", release.version);
            update::apply_staged(paths, &staged, &target, &release.version, logger)
                .await
                .context("apply the DFRelay update")?;
            println!("DFRelay updated to v{}.", release.version);
        }
        UpdateCommand::Rollback => {
            let target = update::target_binary().context("resolve the installed dfr binary")?;
            update::rollback_to_previous(paths, &target, logger)
                .await
                .context("roll back the DFRelay binary")?;
            println!("Restored the previous DFRelay binary.");
        }
        UpdateCommand::Status => {
            let state = update::load_state(paths);
            println!("Current:      {}", version::DISPLAY);
            println!("Policy:       {}", config.app_update.as_str());
            match state.available_update() {
                Some(version) => println!("Latest:       v{version} (update available)"),
                None if state.last_check_unix.is_some() => {
                    println!("Latest:       no newer release");
                }
                None => {}
            }
            if let Some((staged, _)) = update::discover_staged(paths) {
                println!("Staged:       v{staged}");
            }
            println!("Last check:   {}", format_unix(state.last_check_unix));
            println!("Last success: {}", format_unix(state.last_success_unix));
            if let Some(version) = state.failed_version.as_deref() {
                println!(
                    "Failed:       {} ({})",
                    version,
                    format_unix(state.failed_at_unix)
                );
            }
            if let Some(error) = state.last_error.as_deref() {
                println!("Last error:   {error}");
            }
            if !update::is_release_build() {
                println!("Note: local builds do not update automatically.");
            }
        }
        UpdateCommand::Helper {
            staged,
            target,
            version,
        } => {
            let result = update::apply_staged(paths, &staged, &target, &version, logger).await;
            if let Err(err) = &result {
                // Early failures (lock contention, staged integrity, target
                // permissions, stop/install errors) never reach the rollback
                // path, so the daemon would otherwise retry the same staged
                // update forever.
                update::record_helper_failure(paths, &version, err);
            }
            result.context("apply the staged DFRelay update")?;
        }
    }
    Ok(())
}

fn format_unix(timestamp: Option<i64>) -> String {
    timestamp
        .and_then(|value| chrono::Local.timestamp_opt(value, 0).single())
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "—".to_string())
}

fn record_check(paths: &Paths, logger: &Logger, available: Option<&str>) {
    if let Err(err) = update::record_check_result(paths, available) {
        logger.app(
            LogLevel::Warn,
            format!("cannot record the update check: {err}"),
        );
    }
}

async fn print_status(paths: &Paths) -> anyhow::Result<()> {
    let socket = paths.control_socket();
    if !ipc::daemon_is_running(&socket).await {
        println!("DFRelay daemon: not running");
        return Ok(());
    }
    let status = ipc::control_status(&socket)
        .await
        .context("query DFRelay daemon status")?;
    println!("DFRelay daemon: running (pid {})", status.pid);
    let mode = if status.managed {
        match &status.service {
            Some(service) => format!("service-managed ({})", service.summary()),
            None => "service-managed".to_string(),
        }
    } else {
        "ad-hoc".to_string()
    };
    println!("Mode: {mode}");
    println!(
        "Proxy: {} (uptime {})",
        proxy_label(&status.proxy),
        format_uptime(status.uptime_secs)
    );
    println!("Node: {}", status.current_node.as_deref().unwrap_or("-"));
    println!(
        "Listen: {}",
        if status.listen.is_empty() {
            "-"
        } else {
            status.listen.as_str()
        }
    );
    Ok(())
}

async fn report_start(socket: &Path) {
    match wait_for_proxy(socket, Duration::from_secs(20)).await {
        Some(status) if status.proxy == ProxyStatus::Running => {
            println!("Proxy started.");
        }
        Some(status) => {
            println!("Daemon started; proxy is {}.", proxy_label(&status.proxy));
        }
        None => println!("Daemon started; proxy is starting."),
    }
}

async fn wait_for_socket(socket: &Path, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if ipc::daemon_is_running(socket).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("DFRelay daemon did not start; check the log directory")
}

async fn wait_for_socket_gone(socket: &Path, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !ipc::daemon_is_running(socket).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("DFRelay daemon is still running after the service stop request")
}

// The daemon removes its control socket before exiting, so control_shutdown
// returning does not mean the old process has released the flock.
async fn wait_for_daemon_lock(data_dir: &Path, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match ipc::acquire_daemon_lock(data_dir) {
            Ok(lock) => {
                drop(lock);
                return Ok(());
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("DFRelay daemon did not release its startup lock");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => return Err(err.into()),
        }
    }
}

async fn wait_for_new_daemon(
    socket: &Path,
    old_pid: u32,
    timeout: Duration,
) -> anyhow::Result<DaemonStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(status) = ipc::control_status(socket).await {
            if status.pid != old_pid {
                return Ok(status);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("DFRelay daemon did not restart (pid {old_pid} is still reported)")
}

async fn wait_for_proxy(socket: &Path, timeout: Duration) -> Option<DaemonStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(status) = ipc::control_status(socket).await {
            if matches!(&status.proxy, ProxyStatus::Running | ProxyStatus::Failed) {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return Some(status);
            }
        } else if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn proxy_label(status: &ProxyStatus) -> &'static str {
    status.label(Lang::En.text())
}

fn format_uptime(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, Show);
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    #[test]
    fn default_command_is_tui() {
        let cli = parse(&["dfr"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn lifecycle_subcommands_parse() {
        for (arg, expected) in [
            ("start", "start"),
            ("stop", "stop"),
            ("restart", "restart"),
            ("status", "status"),
            ("serve", "serve"),
        ] {
            let cli = parse(&["dfr", arg]).unwrap();
            let actual = match cli.command.unwrap() {
                Command::Start => "start",
                Command::Stop => "stop",
                Command::Restart => "restart",
                Command::Status => "status",
                Command::Serve => "serve",
                Command::Service { .. } => "service",
                Command::Update { .. } => "update",
            };
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn service_forget_subcommand_parses() {
        let cli = parse(&["dfr", "service", "forget"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceCommand::Forget
            })
        ));
    }

    #[test]
    fn update_subcommands_parse() {
        for (args, expected) in [
            (vec!["dfr", "update", "check"], "check"),
            (vec!["dfr", "update", "apply"], "apply"),
            (vec!["dfr", "update", "rollback"], "rollback"),
            (vec!["dfr", "update", "status"], "status"),
        ] {
            let cli = parse(&args).unwrap();
            let actual = match cli.command.unwrap() {
                Command::Update { action } => match action {
                    UpdateCommand::Check => "check",
                    UpdateCommand::Apply => "apply",
                    UpdateCommand::Rollback => "rollback",
                    UpdateCommand::Status => "status",
                    UpdateCommand::Helper { .. } => "helper",
                },
                _ => "other",
            };
            assert_eq!(actual, expected);
        }

        let cli = parse(&[
            "dfr",
            "update",
            "helper",
            "--staged",
            "/tmp/a",
            "--target",
            "/tmp/b",
            "--version",
            "1.2.3",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Update {
                action: UpdateCommand::Helper { .. }
            })
        ));

        let cli = parse(&[
            "dfr",
            "update",
            "helper",
            "--staged",
            "/tmp/a",
            "--target",
            "/tmp/b",
            "--version",
            "1.2.3",
            "--config",
            "/tmp/config.toml",
            "--data",
            "/tmp/data",
        ])
        .unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/config.toml")));
        assert_eq!(cli.data_dir, Some(PathBuf::from("/tmp/data")));
    }

    #[test]
    fn old_flags_are_rejected() {
        for arg in [
            "--daemon",
            "--stop",
            "--restart",
            "--internal-daemon",
            "--data-dir",
        ] {
            assert!(parse(&["dfr", arg]).is_err(), "{arg} should be rejected");
        }
    }

    #[test]
    fn global_paths_apply_to_subcommands() {
        let cli = parse(&["dfr", "serve", "--config", "/tmp/a.toml"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/a.toml")));
        assert!(matches!(cli.command, Some(Command::Serve)));

        let cli = parse(&["dfr", "--data", "/tmp/data", "start"]).unwrap();
        assert_eq!(cli.data_dir, Some(PathBuf::from("/tmp/data")));
        assert!(matches!(cli.command, Some(Command::Start)));
    }

    #[test]
    fn short_path_flags_apply_to_subcommands() {
        let cli = parse(&["dfr", "status", "-c", "/tmp/a.toml"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/a.toml")));
        assert!(matches!(cli.command, Some(Command::Status)));

        let cli = parse(&["dfr", "-d", "/tmp/data", "stop"]).unwrap();
        assert_eq!(cli.data_dir, Some(PathBuf::from("/tmp/data")));
        assert!(matches!(cli.command, Some(Command::Stop)));
    }

    #[test]
    fn uptime_formats_hours_only_when_needed() {
        assert_eq!(format_uptime(9), "00:09");
        assert_eq!(format_uptime(61), "01:01");
        assert_eq!(format_uptime(3601), "01:00:01");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn daemon_lock_wait_returns_after_release() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = ipc::acquire_daemon_lock(dir.path()).unwrap();
        let waiter = tokio::spawn({
            let dir = dir.path().to_path_buf();
            async move { wait_for_daemon_lock(&dir, Duration::from_secs(2)).await }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(lock);
        waiter.await.unwrap().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn daemon_lock_wait_times_out_when_held() {
        let dir = tempfile::TempDir::new().unwrap();
        let _lock = ipc::acquire_daemon_lock(dir.path()).unwrap();
        let err = wait_for_daemon_lock(dir.path(), Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("did not release"), "{err}");
    }
}
