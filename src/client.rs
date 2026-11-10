use std::io::{stdout, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::i18n::Lang;
use crate::ipc::{ClientMsg, InputCode, ServerMsg};
use crate::state::Page;
use crate::theme::Palette;
use crate::view::View;

enum InputEvent {
    Key(ClientMsg),
    Resize,
}

pub async fn run(socket: PathBuf, lang: Lang) -> anyhow::Result<()> {
    let stream = UnixStream::connect(&socket)
        .await
        .context("connect to the DFRelay daemon")?;
    let (read_half, write_half) = stream.into_split();
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let _ = out_tx.send(ClientMsg::Hello { viewer: true });
    tokio::spawn(write_out(write_half, out_rx));

    let note = {
        enable_raw_mode()?;
        let _guard = TerminalGuard;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, Hide)?;
        let backend = CrosstermBackend::new(out);
        let mut terminal = Terminal::new(backend)?;
        let palette = Palette::auto();
        let mut reader = BufReader::new(read_half).lines();
        let (in_tx, mut in_rx) = mpsc::unbounded_channel();
        spawn_input(in_tx);
        let mut view: Option<View> = None;
        let mut parse_errors = 0usize;
        let mut note: Option<&'static str> = None;
        let mut last_help_max: Option<u16> = None;
        let mut last_dashboard_max: Option<u16> = None;
        let mut last_log_detail_max: Option<u16> = None;
        let mut last_page: Option<Page> = None;
        let mut animation_start = Instant::now();
        let mut needs_draw = true;
        let mut animate = false;
        let mut toast_deadline: Option<Instant> = None;

        loop {
            if needs_draw {
                if let Some(current) = &view {
                    if last_page != Some(current.page) {
                        // ratatui 0.30 `clear()` first queries the cursor position, which
                        // fails on terminals that do not answer DSR. Resizing to the same
                        // fullscreen area performs the same clear and buffer reset without
                        // that query.
                        terminal.resize(terminal.size()?.into())?;
                        last_page = Some(current.page);
                        animation_start = Instant::now();
                    }
                }
                let tick = (animation_start.elapsed().as_millis() / 100) as u64;
                let anim = crate::ui::Anim {
                    tick,
                    toast_remaining: toast_deadline
                        .map(|deadline| deadline.saturating_duration_since(Instant::now())),
                };
                terminal.draw(|frame| match &view {
                    Some(view) => crate::ui::render_at(frame, view, anim, palette),
                    None => render_connecting(frame, lang),
                })?;
                if let Some(current) = &view {
                    let size = terminal.size()?;
                    let max_scroll = crate::ui::help_max_scroll(current, size.width, size.height);
                    let max_scroll = u16::try_from(max_scroll).unwrap_or(u16::MAX);
                    if last_help_max != Some(max_scroll) {
                        let _ = out_tx.send(ClientMsg::HelpViewport { max_scroll });
                        last_help_max = Some(max_scroll);
                    }
                    if current.page == Page::Dashboard {
                        let max_scroll =
                            crate::ui::dashboard_max_scroll(current, size.width, size.height);
                        let max_scroll = u16::try_from(max_scroll).unwrap_or(u16::MAX);
                        if last_dashboard_max != Some(max_scroll) {
                            let _ = out_tx.send(ClientMsg::DashboardViewport { max_scroll });
                            last_dashboard_max = Some(max_scroll);
                        }
                    } else {
                        last_dashboard_max = None;
                    }
                    if current.log_detail().is_some() {
                        let max_scroll = crate::ui::log_detail_max_scroll(
                            current,
                            size.width,
                            size.height,
                            palette,
                        );
                        let max_scroll = u16::try_from(max_scroll).unwrap_or(u16::MAX);
                        if last_log_detail_max != Some(max_scroll) {
                            let _ = out_tx.send(ClientMsg::LogDetailViewport { max_scroll });
                            last_log_detail_max = Some(max_scroll);
                        }
                    } else {
                        last_log_detail_max = None;
                    }
                }
                needs_draw = false;
            }
            tokio::select! {
                line = reader.next_line() => {
                    match line {
                        Ok(Some(line)) => match serde_json::from_str::<ServerMsg>(&line) {
                            Ok(ServerMsg::View(frame)) => {
                                if frame.quitting {
                                    break;
                                }
                                animate = crate::ui::needs_animation(&frame);
                                toast_deadline = frame.toast_remaining_ms.map(|remaining| {
                                    Instant::now() + Duration::from_millis(remaining)
                                });
                                view = Some(*frame);
                                needs_draw = true;
                            }
                            Ok(ServerMsg::Busy) => {
                                note = Some(lang.text().msg_attach_busy);
                                break;
                            }
                            Ok(ServerMsg::Detach) => {
                                note = Some(lang.text().msg_detached);
                                break;
                            }
                            Ok(ServerMsg::Shutdown) => break,
                            Ok(ServerMsg::Copy { text }) => copy_to_clipboard(&text),
                            Ok(ServerMsg::Ack { .. }) | Ok(ServerMsg::Status(_)) => {}
                            Err(_) => {
                                parse_errors += 1;
                                if parse_errors >= 3 {
                                    note = Some(lang.text().msg_daemon_incompatible);
                                    break;
                                }
                            }
                        },
                        Ok(None) => {
                            note = Some(if view.is_some() {
                                lang.text().msg_daemon_closed
                            } else {
                                lang.text().msg_daemon_unavailable
                            });
                            break;
                        }
                        Err(_) => {
                            note = Some(lang.text().msg_daemon_closed);
                            break;
                        }
                    }
                }
                Some(message) = in_rx.recv() => {
                    match message {
                        InputEvent::Key(message) => {
                            let _ = out_tx.send(message);
                        }
                        InputEvent::Resize => needs_draw = true,
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)), if animate => {
                    needs_draw = true;
                }
            }
        }
        note
    };

    if let Some(note) = note {
        println!("{note}");
    }
    Ok(())
}

fn copy_to_clipboard(text: &str) {
    let encoded = BASE64.encode(text.as_bytes());
    let mut out = stdout();
    if std::env::var_os("TMUX").is_some() {
        // tmux needs the OSC 52 payload wrapped in DCS passthrough.
        let _ = write!(out, "\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\");
    } else {
        let _ = write!(out, "\x1b]52;c;{encoded}\x07");
    }
    let _ = out.flush();
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, Show);
    }
}

fn spawn_input(tx: mpsc::UnboundedSender<InputEvent>) {
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                let Some(code) = InputCode::from_key_code(key.code) else {
                    continue;
                };
                let message = ClientMsg::Key {
                    code,
                    ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
                    alt: key.modifiers.contains(KeyModifiers::ALT),
                };
                if tx.send(InputEvent::Key(message)).is_err() {
                    break;
                }
            }
            Ok(Event::Resize(_, _)) => {
                if tx.send(InputEvent::Resize).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    });
}

async fn write_out(mut write: OwnedWriteHalf, mut rx: UnboundedReceiver<ClientMsg>) {
    while let Some(message) = rx.recv().await {
        let Ok(mut line) = serde_json::to_vec(&message) else {
            continue;
        };
        line.push(b'\n');
        if write.write_all(&line).await.is_err() {
            break;
        }
    }
}

fn render_connecting(frame: &mut Frame, lang: Lang) {
    let text = lang.text();
    let lines = vec![
        Line::from(Span::styled(
            text.app_title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(text.msg_connecting),
    ];
    let chunks = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Length(3),
        Constraint::Percentage(40),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        chunks[1],
    );
}
