use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::{watch, Semaphore};

use crate::app::{AppEvent, BackgroundTask};
use crate::view::{DaemonStatus, View};

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);
pub const PROTOCOL_VERSION: u32 = 2;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(120);
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_INBOUND_MESSAGES_PER_SECOND: usize = 120;
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

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
        #[serde(default)]
        version: u32,
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
    Incompatible { expected: u32, got: u32 },
}

#[derive(Debug, Clone)]
pub struct Ack {
    pub ok: bool,
    pub message: String,
}

#[derive(Clone)]
pub struct ClientTx {
    control: mpsc::UnboundedSender<ServerMsg>,
    view: watch::Sender<Option<Arc<View>>>,
}

impl ClientTx {
    fn new() -> (
        Self,
        mpsc::UnboundedReceiver<ServerMsg>,
        watch::Receiver<Option<Arc<View>>>,
    ) {
        let (control, control_rx) = mpsc::unbounded_channel();
        let (view, view_rx) = watch::channel(None);
        (Self { control, view }, control_rx, view_rx)
    }

    pub fn try_send(&self, message: ServerMsg) -> Result<(), ()> {
        match message {
            ServerMsg::View(view) => {
                self.view.send_replace(Some(Arc::from(view)));
                Ok(())
            }
            message => self.control.send(message).map_err(|_| ()),
        }
    }

    pub fn send_view(&self, view: Arc<View>) {
        self.view.send_replace(Some(view));
    }

    #[cfg(test)]
    fn send(&self, message: ServerMsg) -> Result<(), ()> {
        self.try_send(message)
    }

    #[cfg(test)]
    pub(crate) fn test_channel() -> (Self, mpsc::UnboundedReceiver<ServerMsg>) {
        let (control, rx) = mpsc::unbounded_channel();
        let (view, _) = watch::channel(None);
        (Self { control, view }, rx)
    }
}

pub fn spawn_listener(listener: UnixListener, events_tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let slots = Arc::new(Semaphore::new(crate::app::MAX_CLIENTS + 8));
        loop {
            match crate::guard::CatchUnwind::new(listener.accept()).await {
                Ok(Ok((stream, _))) => {
                    let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                        continue;
                    };
                    let events_tx = events_tx.clone();
                    let id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        let _permit = permit;
                        if crate::guard::CatchUnwind::new(handle_client(
                            stream,
                            events_tx.clone(),
                            id,
                        ))
                        .await
                        .is_err()
                        {
                            let _ = events_tx.send(AppEvent::ClientDetached(id));
                        }
                    });
                }
                Ok(Err(_)) => {
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
                Err(payload) => {
                    let message = crate::guard::panic_message(payload.as_ref());
                    let _ = events_tx.send(AppEvent::BackgroundPanic {
                        task: BackgroundTask::Ipc,
                        message,
                    });
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            }
        }
    });
}

async fn next_line_limited<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8"));
        }
        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            if bytes.len() + position > max {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
            }
            bytes.extend_from_slice(&available[..position]);
            reader.consume(position + 1);
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8"));
        }
        if bytes.len() + available.len() > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        let length = available.len();
        bytes.extend_from_slice(available);
        reader.consume(length);
    }
}

async fn handle_client(stream: UnixStream, events_tx: UnboundedSender<AppEvent>, id: u64) {
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let viewer = match tokio::time::timeout(
        HELLO_TIMEOUT,
        next_line_limited(&mut reader, MAX_LINE_BYTES),
    )
    .await
    {
        Ok(Ok(Some(line))) => match serde_json::from_str::<ClientMsg>(&line) {
            Ok(ClientMsg::Hello { viewer, version }) if version == PROTOCOL_VERSION => viewer,
            Ok(ClientMsg::Hello { version, .. }) => {
                reject_incompatible(write_half, version).await;
                return;
            }
            _ => return,
        },
        _ => return,
    };
    let (client_tx, control_rx, view_rx) = ClientTx::new();
    let write_events = events_tx.clone();
    tokio::spawn(async move {
        if crate::guard::CatchUnwind::new(write_client(
            write_half,
            control_rx,
            view_rx,
            write_events.clone(),
            id,
        ))
        .await
        .is_err()
        {
            let _ = write_events.send(AppEvent::ClientDetached(id));
        }
    });
    let (accept_tx, accept_rx) = tokio::sync::oneshot::channel();
    if events_tx
        .send(AppEvent::ClientAttached {
            id,
            viewer,
            tx: client_tx,
            accept: accept_tx,
        })
        .is_err()
    {
        return;
    }
    match tokio::time::timeout(HELLO_TIMEOUT, accept_rx).await {
        Ok(Ok(true)) => {}
        _ => {
            let _ = events_tx.send(AppEvent::ClientDetached(id));
            return;
        }
    }
    let mut rate_window = std::time::Instant::now();
    let mut rate_count = 0usize;
    loop {
        let next = if viewer {
            next_line_limited(&mut reader, MAX_LINE_BYTES).await
        } else {
            match tokio::time::timeout(
                CONTROL_IDLE_TIMEOUT,
                next_line_limited(&mut reader, MAX_LINE_BYTES),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => break,
            }
        };
        let Ok(Some(line)) = next else {
            break;
        };
        if rate_window.elapsed() >= Duration::from_secs(1) {
            rate_window = std::time::Instant::now();
            rate_count = 0;
        }
        if rate_count >= MAX_INBOUND_MESSAGES_PER_SECOND {
            continue;
        }
        rate_count += 1;
        let Ok(message) = serde_json::from_str::<ClientMsg>(&line) else {
            continue;
        };
        match message {
            ClientMsg::Hello { .. } => {}
            ClientMsg::Key { code, ctrl, alt } if viewer => {
                let _ = events_tx.send(AppEvent::Key(code.to_key_event(ctrl, alt)));
            }
            ClientMsg::HelpViewport { max_scroll } if viewer => {
                let _ = events_tx.send(AppEvent::HelpViewport(max_scroll as usize));
            }
            ClientMsg::DashboardViewport { max_scroll } if viewer => {
                let _ = events_tx.send(AppEvent::DashboardViewport(max_scroll as usize));
            }
            ClientMsg::LogDetailViewport { max_scroll } if viewer => {
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
            _ => {}
        }
    }
    let _ = events_tx.send(AppEvent::ClientDetached(id));
}

async fn write_client(
    mut write: OwnedWriteHalf,
    mut control: mpsc::UnboundedReceiver<ServerMsg>,
    mut view: watch::Receiver<Option<Arc<View>>>,
    events_tx: UnboundedSender<AppEvent>,
    id: u64,
) {
    loop {
        let message = tokio::select! {
            biased;
            message = control.recv() => match message {
                Some(message) => message,
                None => break,
            },
            changed = view.changed() => {
                if changed.is_err() {
                    break;
                }
                let Some(frame) = view.borrow_and_update().as_ref().cloned() else {
                    continue;
                };
                ServerMsg::View(Box::new((*frame).clone()))
            }
        };
        let Ok(mut line) = serde_json::to_vec(&message) else {
            continue;
        };
        line.push(b'\n');
        // A viewer that stops reading must not hold the only viewer slot forever.
        match tokio::time::timeout(WRITE_TIMEOUT, write.write_all(&line)).await {
            Ok(Ok(())) => {}
            _ => {
                let _ = events_tx.send(AppEvent::ClientDetached(id));
                break;
            }
        }
    }
}

// SECURITY: bind 0600 under umask, hold the daemon flock, and only unlink a socket file at the control path.
fn bind_private(socket: &Path) -> io::Result<UnixListener> {
    #[cfg(unix)]
    // SAFETY: umask has no preconditions; the process-global mode is restored immediately after bind.
    let previous = unsafe { libc::umask(0o077) };
    let result = UnixListener::bind(socket);
    #[cfg(unix)]
    // SAFETY: restores the mode saved above; umask has no preconditions.
    unsafe {
        libc::umask(previous);
    }
    result
}

pub fn acquire_daemon_lock(data_dir: &Path) -> io::Result<std::fs::File> {
    crate::fsutil::ensure_owned_private_dir(data_dir)?;
    let path = data_dir.join("daemon.lock");
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let file = crate::fsutil::open_private_file(&path, false)?;
        // SAFETY: file owns the descriptor for this scope; flock does not retain it after the call.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if locked {
            Ok(file)
        } else {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "another DFRelay daemon is running",
            ))
        }
    }
    #[cfg(not(unix))]
    {
        crate::fsutil::open_private_file(&path, false)
    }
}

pub async fn bind_listener(socket: &Path) -> io::Result<UnixListener> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if socket.as_os_str().as_bytes().len() >= 100 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "control socket path is too long for a Unix socket: {}",
                    socket.display()
                ),
            ));
        }
    }
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match bind_private(socket) {
        Ok(listener) => {
            set_socket_permissions(socket)?;
            Ok(listener)
        }
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            match UnixStream::connect(socket).await {
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "daemon already running",
                )),
                Err(connect_err)
                    if matches!(
                        connect_err.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    match std::fs::symlink_metadata(socket) {
                        Ok(metadata) if metadata.file_type().is_socket() => {
                            std::fs::remove_file(socket)?;
                        }
                        Ok(_) => {
                            return Err(io::Error::new(
                                io::ErrorKind::AddrInUse,
                                "control socket path exists and is not a socket",
                            ));
                        }
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err),
                    }
                    let listener = bind_private(socket)?;
                    set_socket_permissions(socket)?;
                    Ok(listener)
                }
                Err(connect_err) => Err(connect_err),
            }
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
    spawn_daemon_at(&std::env::current_exe()?, config, data_dir)
}

pub fn spawn_daemon_at(
    executable: &Path,
    config: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> io::Result<()> {
    let mut command = Command::new(executable);
    command.arg("serve");
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    if let Some(data_dir) = data_dir {
        command.arg("--data").arg(data_dir);
    }
    command.env_remove("DFRELAY_SERVICE");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec runs after fork before exec; the closure only calls setsid and allocates or locks nothing.
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

async fn open_control(
    socket: &Path,
) -> io::Result<(tokio::io::Lines<BufReader<OwnedReadHalf>>, OwnedWriteHalf)> {
    let stream = UnixStream::connect(socket).await?;
    let (read_half, mut write_half) = stream.into_split();
    send(
        &mut write_half,
        &ClientMsg::Hello {
            viewer: false,
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    Ok((BufReader::new(read_half).lines(), write_half))
}

async fn read_control_line(
    lines: &mut tokio::io::Lines<BufReader<OwnedReadHalf>>,
    timeout: Duration,
    timeout_message: &'static str,
    closed_message: &'static str,
) -> io::Result<String> {
    let line = tokio::time::timeout(timeout, lines.next_line())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, timeout_message))??;
    line.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, closed_message))
}

pub async fn control_start(socket: &Path) -> io::Result<Ack> {
    let (mut lines, mut write_half) = open_control(socket).await?;
    send(&mut write_half, &ClientMsg::StartProxy).await?;
    loop {
        let line = read_control_line(
            &mut lines,
            CONTROL_TIMEOUT,
            "daemon did not answer",
            "daemon closed the control connection",
        )
        .await?;
        match serde_json::from_str::<ServerMsg>(&line) {
            Ok(ServerMsg::Ack { ok, message }) => return Ok(Ack { ok, message }),
            Ok(ServerMsg::Incompatible { expected, got }) => {
                return Err(incompatible_error(expected, got));
            }
            _ => {}
        }
    }
}

pub async fn control_status(socket: &Path) -> io::Result<DaemonStatus> {
    let (mut lines, mut write_half) = open_control(socket).await?;
    send(&mut write_half, &ClientMsg::Status).await?;
    loop {
        let line = read_control_line(
            &mut lines,
            STATUS_TIMEOUT,
            "daemon did not answer status",
            "daemon closed the control connection",
        )
        .await?;
        match serde_json::from_str::<ServerMsg>(&line) {
            Ok(ServerMsg::Status(status)) => return Ok(*status),
            Ok(ServerMsg::Incompatible { expected, got }) => {
                return Err(incompatible_error(expected, got));
            }
            _ => {}
        }
    }
}

pub async fn control_shutdown(socket: &Path) -> io::Result<()> {
    let (mut lines, mut write_half) = open_control(socket).await?;
    send(&mut write_half, &ClientMsg::Shutdown).await?;
    loop {
        let line = read_control_line(
            &mut lines,
            CONTROL_TIMEOUT,
            "daemon did not acknowledge shutdown",
            "daemon closed the control connection before acknowledging shutdown",
        )
        .await?;
        match serde_json::from_str::<ServerMsg>(&line) {
            Ok(ServerMsg::Ack { .. }) => break,
            Ok(ServerMsg::Incompatible { expected, got }) => {
                return Err(incompatible_error(expected, got));
            }
            _ => {}
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

async fn reject_incompatible(mut write: OwnedWriteHalf, got: u32) {
    let Ok(mut line) = serde_json::to_vec(&ServerMsg::Incompatible {
        expected: PROTOCOL_VERSION,
        got,
    }) else {
        return;
    };
    line.push(b'\n');
    for _ in 0..3 {
        if write.write_all(&line).await.is_err() {
            break;
        }
    }
}

async fn send(write: &mut OwnedWriteHalf, message: &ClientMsg) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    line.push(b'\n');
    write.write_all(&line).await
}

fn incompatible_error(daemon_version: u32, client_version: u32) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "daemon protocol {daemon_version} does not match client protocol {client_version}; restart the daemon"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ProxyStatus;

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
            version: "1.0.0".into(),
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
    fn hello_carries_the_protocol_version() {
        let line = serde_json::to_string(&ClientMsg::Hello {
            viewer: true,
            version: PROTOCOL_VERSION,
        })
        .unwrap();
        let parsed: ClientMsg = serde_json::from_str(&line).unwrap();
        match parsed {
            ClientMsg::Hello { viewer, version } => {
                assert!(viewer);
                assert_eq!(version, PROTOCOL_VERSION);
            }
            _ => panic!("expected hello"),
        }
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

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_control_socket_paths_that_are_too_long() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("x".repeat(120));
        let err = bind_listener(&socket).await.unwrap_err();
        assert!(err.to_string().contains("too long"), "{err}");
    }

    #[test]
    fn daemon_lock_rejects_a_second_owner() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = acquire_daemon_lock(dir.path()).unwrap();
        let err = acquire_daemon_lock(dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(first);
        assert!(acquire_daemon_lock(dir.path()).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_listener_does_not_delete_a_regular_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("dfr.sock");
        std::fs::write(&socket, b"user data").unwrap();
        let err = bind_listener(&socket).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert_eq!(std::fs::read(&socket).unwrap(), b"user data");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_listener_replaces_a_dead_socket() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("dfr.sock");
        let stale = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        drop(stale);
        assert!(bind_listener(&socket).await.is_ok());
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

    #[cfg(unix)]
    async fn test_listener() -> (
        tempfile::TempDir,
        PathBuf,
        mpsc::UnboundedReceiver<AppEvent>,
    ) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        spawn_listener(listener, tx);
        (dir, path, rx)
    }

    #[cfg(unix)]
    async fn next_event(rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> AppEvent {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("daemon event timeout")
            .expect("daemon event channel closed")
    }

    #[cfg(unix)]
    async fn hello(write: &mut OwnedWriteHalf, viewer: bool, version: u32) {
        send(write, &ClientMsg::Hello { viewer, version })
            .await
            .unwrap();
    }

    #[cfg(unix)]
    async fn read_line(
        lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    ) -> String {
        tokio::time::timeout(Duration::from_secs(2), lines.next_line())
            .await
            .expect("server response timeout")
            .expect("server read failed")
            .expect("server closed the connection")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn viewer_hello_forwards_keys_and_copy() {
        let (_dir, path, mut events) = test_listener().await;
        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        hello(&mut write, true, PROTOCOL_VERSION).await;

        let (id, tx, accept) = match next_event(&mut events).await {
            AppEvent::ClientAttached {
                id,
                viewer,
                tx,
                accept,
            } => {
                assert!(viewer);
                (id, tx, accept)
            }
            _ => panic!("expected ClientAttached"),
        };
        accept.send(true).unwrap();

        tx.send(ServerMsg::Copy {
            text: "copied".into(),
        })
        .unwrap();
        let line = read_line(&mut lines).await;
        assert!(matches!(
            serde_json::from_str::<ServerMsg>(&line).unwrap(),
            ServerMsg::Copy { text } if text == "copied"
        ));

        send(
            &mut write,
            &ClientMsg::Key {
                code: InputCode::Char('j'),
                ctrl: false,
                alt: false,
            },
        )
        .await
        .unwrap();
        match next_event(&mut events).await {
            AppEvent::Key(key) => assert_eq!(key.code, KeyCode::Char('j')),
            _ => panic!("expected Key event"),
        }

        send(&mut write, &ClientMsg::Shutdown).await.unwrap();
        match next_event(&mut events).await {
            AppEvent::ShutdownRequest { client_id } => assert_eq!(client_id, id),
            _ => panic!("expected ShutdownRequest"),
        }

        drop(write);
        drop(lines);
        match next_event(&mut events).await {
            AppEvent::ClientDetached(detached) => assert_eq!(detached, id),
            _ => panic!("expected ClientDetached"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn second_viewer_is_rejected_with_busy() {
        let (_dir, path, mut events) = test_listener().await;
        let first = UnixStream::connect(&path).await.unwrap();
        let (first_read, mut first_write) = first.into_split();
        let first_lines = BufReader::new(first_read).lines();
        hello(&mut first_write, true, PROTOCOL_VERSION).await;
        let (first_id, _first_tx, accept) = match next_event(&mut events).await {
            AppEvent::ClientAttached { id, tx, accept, .. } => (id, tx, accept),
            _ => panic!("expected first ClientAttached"),
        };
        accept.send(true).unwrap();

        let second = UnixStream::connect(&path).await.unwrap();
        let (second_read, mut second_write) = second.into_split();
        let mut second_lines = BufReader::new(second_read).lines();
        hello(&mut second_write, true, PROTOCOL_VERSION).await;
        let (second_id, second_tx, accept) = match next_event(&mut events).await {
            AppEvent::ClientAttached { id, tx, accept, .. } => (id, tx, accept),
            _ => panic!("expected second ClientAttached"),
        };
        accept.send(false).unwrap();
        second_tx.send(ServerMsg::Busy).unwrap();
        let line = read_line(&mut second_lines).await;
        assert!(matches!(
            serde_json::from_str::<ServerMsg>(&line).unwrap(),
            ServerMsg::Busy
        ));
        assert_ne!(first_id, second_id);

        drop(first_write);
        drop(first_lines);
        loop {
            match next_event(&mut events).await {
                AppEvent::ClientDetached(id) if id == first_id => break,
                AppEvent::ClientDetached(id) => assert_eq!(id, second_id),
                _ => panic!("expected first ClientDetached"),
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn control_messages_preserve_order_and_count() {
        let (client, server) = UnixStream::pair().unwrap();
        let (server_read, server_write) = server.into_split();
        let (control, control_rx) = mpsc::unbounded_channel();
        let (view, view_rx) = watch::channel(None);
        let tx = ClientTx { control, view };
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        let writer = tokio::spawn(write_client(
            server_write,
            control_rx,
            view_rx,
            events_tx.clone(),
            7,
        ));
        let (client_read, _client_write) = client.into_split();
        let mut lines = BufReader::new(client_read).lines();

        for index in 0..64 {
            tx.send(ServerMsg::Copy {
                text: format!("copy-{index}"),
            })
            .unwrap();
        }
        tx.send(ServerMsg::Detach).unwrap();
        for index in 0..64 {
            let line = read_line(&mut lines).await;
            assert!(matches!(
                serde_json::from_str::<ServerMsg>(&line).unwrap(),
                ServerMsg::Copy { text } if text == format!("copy-{index}")
            ));
        }
        let line = read_line(&mut lines).await;
        assert!(matches!(
            serde_json::from_str::<ServerMsg>(&line).unwrap(),
            ServerMsg::Detach
        ));
        drop(tx);
        writer.await.unwrap();
        drop(server_read);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejected_accept_requests_detach() {
        let (_dir, path, mut events) = test_listener().await;
        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let _lines = BufReader::new(read).lines();
        hello(&mut write, true, PROTOCOL_VERSION).await;

        let (id, _tx, accept) = match next_event(&mut events).await {
            AppEvent::ClientAttached { id, tx, accept, .. } => (id, tx, accept),
            _ => panic!("expected ClientAttached"),
        };
        drop(accept);
        match next_event(&mut events).await {
            AppEvent::ClientDetached(detached) => assert_eq!(detached, id),
            _ => panic!("expected ClientDetached"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn control_client_receives_status() {
        let (_dir, path, mut events) = test_listener().await;
        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        hello(&mut write, false, PROTOCOL_VERSION).await;

        let (id, tx, accept) = match next_event(&mut events).await {
            AppEvent::ClientAttached {
                id,
                viewer,
                tx,
                accept,
                ..
            } => {
                assert!(!viewer);
                (id, tx, accept)
            }
            _ => panic!("expected ClientAttached"),
        };
        accept.send(true).unwrap();
        send(&mut write, &ClientMsg::Status).await.unwrap();
        match next_event(&mut events).await {
            AppEvent::StatusRequest { client_id } => assert_eq!(client_id, id),
            _ => panic!("expected StatusRequest"),
        }

        tx.send(ServerMsg::Status(Box::new(DaemonStatus {
            pid: 42,
            managed: false,
            service: None,
            proxy: ProxyStatus::Running,
            uptime_secs: 1,
            listen: "127.0.0.1:12000".into(),
            current_node: Some("node".into()),
            version: "1.0.0".into(),
        })))
        .unwrap();
        let line = read_line(&mut lines).await;
        match serde_json::from_str::<ServerMsg>(&line).unwrap() {
            ServerMsg::Status(status) => {
                assert_eq!(status.pid, 42);
                assert_eq!(status.proxy, ProxyStatus::Running);
            }
            _ => panic!("expected Status response"),
        }
    }

    #[cfg(unix)]
    async fn expect_incompatible(
        lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
        got: u32,
    ) {
        for _ in 0..3 {
            let line = read_line(lines).await;
            match serde_json::from_str::<ServerMsg>(&line).unwrap() {
                ServerMsg::Incompatible {
                    expected,
                    got: actual,
                } => {
                    assert_eq!(expected, PROTOCOL_VERSION);
                    assert_eq!(actual, got);
                }
                _ => panic!("expected Incompatible message"),
            }
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(1), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .is_none(),
            "incompatible client must be disconnected"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hello_version_mismatch_is_rejected() {
        let (_dir, path, mut events) = test_listener().await;
        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        hello(&mut write, true, PROTOCOL_VERSION + 1).await;

        expect_incompatible(&mut lines, PROTOCOL_VERSION + 1).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), events.recv())
                .await
                .is_err(),
            "incompatible client must not be accepted"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_hello_version_is_rejected() {
        let (_dir, path, mut events) = test_listener().await;
        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        write
            .write_all(b"{\"Hello\":{\"viewer\":true}}\n")
            .await
            .unwrap();

        expect_incompatible(&mut lines, 0).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), events.recv())
                .await
                .is_err(),
            "client without a protocol version must not be accepted"
        );
    }

    #[test]
    fn incompatible_error_labels_daemon_and_client_versions() {
        let message = incompatible_error(7, 3).to_string();
        assert!(message.contains("daemon protocol 7"), "{message}");
        assert!(message.contains("client protocol 3"), "{message}");
    }
}
