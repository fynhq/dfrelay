use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::AppEvent;
use crate::view::{DaemonStatus, View};

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(120);
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Delete,
    Home,
    End,
}

impl InputCode {
    pub fn from_key_code(code: KeyCode) -> Option<Self> {
        match code {
            KeyCode::Char(value) => Some(Self::Char(value)),
            KeyCode::Enter => Some(Self::Enter),
            KeyCode::Esc => Some(Self::Esc),
            KeyCode::Tab => Some(Self::Tab),
            KeyCode::BackTab => Some(Self::BackTab),
            KeyCode::Backspace => Some(Self::Backspace),
            KeyCode::Up => Some(Self::Up),
            KeyCode::Down => Some(Self::Down),
            KeyCode::Left => Some(Self::Left),
            KeyCode::Right => Some(Self::Right),
            KeyCode::Delete => Some(Self::Delete),
            KeyCode::Home => Some(Self::Home),
            KeyCode::End => Some(Self::End),
            _ => None,
        }
    }

    pub fn to_key_event(self, ctrl: bool, alt: bool) -> KeyEvent {
        let mut modifiers = KeyModifiers::NONE;
        if ctrl {
            modifiers |= KeyModifiers::CONTROL;
        }
        if alt {
            modifiers |= KeyModifiers::ALT;
        }
        let code = match self {
            Self::Char(value) => KeyCode::Char(value),
            Self::Enter => KeyCode::Enter,
            Self::Esc => KeyCode::Esc,
            Self::Tab => KeyCode::Tab,
            Self::BackTab => KeyCode::BackTab,
            Self::Backspace => KeyCode::Backspace,
            Self::Up => KeyCode::Up,
            Self::Down => KeyCode::Down,
            Self::Left => KeyCode::Left,
            Self::Right => KeyCode::Right,
            Self::Delete => KeyCode::Delete,
            Self::Home => KeyCode::Home,
            Self::End => KeyCode::End,
        };
        KeyEvent::new(code, modifiers)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello {
        viewer: bool,
    },
    Key {
        code: InputCode,
        ctrl: bool,
        alt: bool,
    },
    HelpViewport {
        max_scroll: u16,
    },
    DashboardViewport {
        max_scroll: u16,
    },
    LogDetailViewport {
        max_scroll: u16,
    },
    StartProxy,
    Status,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    View(Box<View>),
    Busy,
    Detach,
    Shutdown,
    Copy { text: String },
    Ack { ok: bool, message: String },
    Status(Box<DaemonStatus>),
}

#[derive(Debug, Clone)]
pub struct Ack {
    pub ok: bool,
    pub message: String,
}

pub fn spawn_listener(listener: UnixListener, events_tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let events_tx = events_tx.clone();
            tokio::spawn(handle_client(stream, events_tx));
        }
    });
}

async fn handle_client(stream: UnixStream, events_tx: UnboundedSender<AppEvent>) {
    let (read_half, write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let viewer = match lines.next_line().await {
        Ok(Some(line)) => match serde_json::from_str::<ClientMsg>(&line) {
            Ok(ClientMsg::Hello { viewer }) => viewer,
            _ => return,
        },
        _ => return,
    };
    let id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
    let (out_tx, out_rx) = mpsc::channel(crate::app::VIEW_QUEUE_LIMIT);
    tokio::spawn(write_client(write_half, out_rx, events_tx.clone(), id));
    if events_tx
        .send(AppEvent::ClientAttached {
            id,
            viewer,
            tx: out_tx,
        })
        .is_err()
    {
        return;
    }
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<ClientMsg>(&line) else {
            continue;
        };
        match message {
            ClientMsg::Hello { .. } => {}
            ClientMsg::Key { code, ctrl, alt } => {
                let _ = events_tx.send(AppEvent::Key(code.to_key_event(ctrl, alt)));
            }
            ClientMsg::HelpViewport { max_scroll } => {
                let _ = events_tx.send(AppEvent::HelpViewport(max_scroll as usize));
            }
            ClientMsg::DashboardViewport { max_scroll } => {
                let _ = events_tx.send(AppEvent::DashboardViewport(max_scroll as usize));
            }
            ClientMsg::LogDetailViewport { max_scroll } => {
                let _ = events_tx.send(AppEvent::LogDetailViewport(max_scroll as usize));
            }
            ClientMsg::StartProxy => {
                let _ = events_tx.send(AppEvent::StartProxyRequest { client_id: id });
            }
            ClientMsg::Status => {
                let _ = events_tx.send(AppEvent::StatusRequest { client_id: id });
            }
            ClientMsg::Shutdown => {
                let _ = events_tx.send(AppEvent::ShutdownRequest { client_id: id });
            }
        }
    }
    let _ = events_tx.send(AppEvent::ClientDetached(id));
}

async fn write_client(
    mut write: OwnedWriteHalf,
    mut rx: mpsc::Receiver<ServerMsg>,
    events_tx: UnboundedSender<AppEvent>,
    id: u64,
) {
    while let Some(message) = rx.recv().await {
        let Ok(mut line) = serde_json::to_vec(&message) else {
            continue;
        };
        line.push(b'\n');
        if write.write_all(&line).await.is_err() {
            let _ = events_tx.send(AppEvent::ClientDetached(id));
            break;
        }
    }
}

pub async fn bind_listener(socket: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match UnixListener::bind(socket) {
        Ok(listener) => {
            set_socket_permissions(socket)?;
            Ok(listener)
        }
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(socket).await.is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "daemon already running",
                ));
            }
            let _ = std::fs::remove_file(socket);
            let listener = UnixListener::bind(socket)?;
            set_socket_permissions(socket)?;
            Ok(listener)
        }
        Err(err) => Err(err),
    }
}

fn set_socket_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn spawn_daemon(config: Option<PathBuf>, data_dir: Option<PathBuf>) -> io::Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("serve");
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    if let Some(data_dir) = data_dir {
        command.arg("--data").arg(data_dir);
    }
    // `start` is an ad-hoc launch; never inherit a service-manager marker.
    command.env_remove("DFRELAY_SERVICE");
    command
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
    Ok(())
}

pub async fn daemon_is_running(socket: &Path) -> bool {
    UnixStream::connect(socket).await.is_ok()
}

pub async fn control_start(socket: &Path) -> io::Result<Ack> {
    let stream = UnixStream::connect(socket).await?;
    let (read_half, mut write_half) = stream.into_split();
    send(&mut write_half, &ClientMsg::Hello { viewer: false }).await?;
    send(&mut write_half, &ClientMsg::StartProxy).await?;
    let mut lines = BufReader::new(read_half).lines();
    loop {
        let line = tokio::time::timeout(CONTROL_TIMEOUT, lines.next_line())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "daemon did not answer"))??;
        let Some(line) = line else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "daemon closed the control connection",
            ));
        };
        if let Ok(ServerMsg::Ack { ok, message }) = serde_json::from_str::<ServerMsg>(&line) {
            return Ok(Ack { ok, message });
        }
    }
}

pub async fn control_status(socket: &Path) -> io::Result<DaemonStatus> {
    let stream = UnixStream::connect(socket).await?;
    let (read_half, mut write_half) = stream.into_split();
    send(&mut write_half, &ClientMsg::Hello { viewer: false }).await?;
    send(&mut write_half, &ClientMsg::Status).await?;
    let mut lines = BufReader::new(read_half).lines();
    loop {
        let line = tokio::time::timeout(STATUS_TIMEOUT, lines.next_line())
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "daemon did not answer status")
            })??;
        let Some(line) = line else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "daemon closed the control connection",
            ));
        };
        if let Ok(ServerMsg::Status(status)) = serde_json::from_str::<ServerMsg>(&line) {
            return Ok(*status);
        }
    }
}

pub async fn control_shutdown(socket: &Path) -> io::Result<()> {
    let stream = UnixStream::connect(socket).await?;
    let (read_half, mut write_half) = stream.into_split();
    send(&mut write_half, &ClientMsg::Hello { viewer: false }).await?;
    send(&mut write_half, &ClientMsg::Shutdown).await?;
    let mut lines = BufReader::new(read_half).lines();
    loop {
        match tokio::time::timeout(CONTROL_TIMEOUT, lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                if matches!(
                    serde_json::from_str::<ServerMsg>(&line),
                    Ok(ServerMsg::Ack { .. })
                ) {
                    break;
                }
            }
            Ok(Ok(None)) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "daemon closed the control connection before acknowledging shutdown",
                ));
            }
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "daemon did not acknowledge shutdown",
                ));
            }
            Ok(Err(err)) => return Err(err),
        }
    }
    let deadline = tokio::time::Instant::now() + SHUTDOWN_WAIT;
    while tokio::time::Instant::now() < deadline {
        if !daemon_is_running(socket).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "daemon is still running after shutdown",
    ))
}

async fn send(write: &mut OwnedWriteHalf, message: &ClientMsg) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    line.push(b'\n');
    write.write_all(&line).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ProxyStatus;

    #[test]
    fn status_protocol_roundtrip() {
        let line = serde_json::to_string(&ClientMsg::Status).unwrap();
        assert!(matches!(
            serde_json::from_str::<ClientMsg>(&line).unwrap(),
            ClientMsg::Status
        ));

        let status = DaemonStatus {
            pid: 42,
            managed: true,
            service: None,
            proxy: ProxyStatus::Running,
            uptime_secs: 61,
            listen: "127.0.0.1:12000".into(),
            current_node: Some("node".into()),
        };
        let line = serde_json::to_string(&ServerMsg::Status(Box::new(status))).unwrap();
        let Ok(ServerMsg::Status(status)) = serde_json::from_str::<ServerMsg>(&line) else {
            panic!("expected status response");
        };
        assert_eq!(status.pid, 42);
        assert!(status.managed);
        assert_eq!(status.proxy, ProxyStatus::Running);
    }

    #[test]
    fn key_codes_roundtrip() {
        for code in [
            KeyCode::Char('q'),
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Up,
            KeyCode::Left,
        ] {
            let input = InputCode::from_key_code(code).unwrap();
            let event = input.to_key_event(true, false);
            assert_eq!(event.code, code);
            assert!(event.modifiers.contains(KeyModifiers::CONTROL));
        }
        assert!(InputCode::from_key_code(KeyCode::F(1)).is_none());
    }

    #[test]
    fn protocol_serializes() {
        let message = ClientMsg::Key {
            code: InputCode::Char('j'),
            ctrl: false,
            alt: false,
        };
        let line = serde_json::to_string(&message).unwrap();
        let parsed: ClientMsg = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            parsed,
            ClientMsg::Key {
                code: InputCode::Char('j'),
                ..
            }
        ));
    }

    #[test]
    fn help_viewport_and_copy_roundtrip() {
        let line = serde_json::to_string(&ClientMsg::HelpViewport { max_scroll: 12 }).unwrap();
        let parsed: ClientMsg = serde_json::from_str(&line).unwrap();
        assert!(matches!(parsed, ClientMsg::HelpViewport { max_scroll: 12 }));

        let line = serde_json::to_string(&ClientMsg::DashboardViewport { max_scroll: 7 }).unwrap();
        let parsed: ClientMsg = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            parsed,
            ClientMsg::DashboardViewport { max_scroll: 7 }
        ));

        let line = serde_json::to_string(&ClientMsg::LogDetailViewport { max_scroll: 5 }).unwrap();
        let parsed: ClientMsg = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            parsed,
            ClientMsg::LogDetailViewport { max_scroll: 5 }
        ));

        let line = serde_json::to_string(&ServerMsg::Copy {
            text: "/tmp/a".into(),
        })
        .unwrap();
        let parsed: ServerMsg = serde_json::from_str(&line).unwrap();
        assert!(matches!(parsed, ServerMsg::Copy { text } if text == "/tmp/a"));
    }
}
