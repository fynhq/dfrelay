use std::time::{Duration, Instant};

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Padding, Paragraph, Row, Table,
    TableState, Tabs, Wrap,
};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::i18n::Text;
use crate::logging::{LogLevel, LogLine, LogSource};
use crate::state::{
    settings_items, EditForm, KeywordForm, LineInput, Overlay, Page, ProxyStatus, SettingsField,
    SettingsForm, SettingsItem, SubscriptionForm, MAIN_PAGES,
};
use crate::theme::Palette;
use crate::view::{BinaryStatus, View};

const HEADER_HEIGHT: u16 = 4;
const FAST_TICK: Duration = Duration::from_millis(100);
pub const STATUS_PULSE: Duration = Duration::from_millis(800);
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PROGRESS_BAR_WIDTH: usize = 12;

mod format;
use format::*;

/// Whether the current snapshot contains visible motion that needs periodic
/// redraws. Idle pages stay static.
pub fn needs_animation(view: &View) -> bool {
    let setup_busy = matches!(view.overlay, Some(Overlay::Setup { .. }))
        && (view.setup.downloading || view.setup.installing || view.setup.rollback_in_flight);
    matches!(view.page, Page::Nodes | Page::Subscriptions)
        || view.restart_in_progress
        || matches!(view.status, ProxyStatus::Starting | ProxyStatus::Stopping)
        || setup_busy
}

/// Redraw cadence for the current snapshot. Fast effects tick at 100 ms, the
/// running indicator pulses at 800 ms, and everything else stays static.
pub fn animation_period(view: &View) -> Option<Duration> {
    if needs_animation(view) {
        Some(FAST_TICK)
    } else if view.status == ProxyStatus::Running {
        Some(STATUS_PULSE)
    } else {
        None
    }
}

/// Next redraw deadline. The running pulse stays on its 800 ms grid even when
/// view snapshots trigger extra draws in between.
pub fn next_animation_deadline(period: Duration, status_start: Instant, now: Instant) -> Instant {
    if period == STATUS_PULSE {
        let elapsed = now.duration_since(status_start).as_millis();
        let remainder = elapsed % STATUS_PULSE.as_millis();
        now + Duration::from_millis((STATUS_PULSE.as_millis() - remainder) as u64)
    } else {
        now + period
    }
}

pub fn status_pulse_green(elapsed: Duration) -> bool {
    (elapsed.as_millis() / STATUS_PULSE.as_millis()) % 2 == 1
}

fn status_dot_style(green: bool, palette: Palette) -> Style {
    if green {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(palette.muted())
    }
}

fn spinner_frame(tick: u64) -> &'static str {
    SPINNER_FRAMES[(tick % SPINNER_FRAMES.len() as u64) as usize]
}

/// Position and length of the filled segment of the indeterminate download
/// bar. The segment ping-pongs across the track.
fn progress_fill(tick: u64, width: usize) -> (usize, usize) {
    let width = width.max(4);
    let fill = (width / 3).max(2).min(width);
    let span = width - fill;
    if span == 0 {
        return (0, fill);
    }
    let cycle = tick as usize % (span * 2);
    let offset = if cycle <= span {
        cycle
    } else {
        span * 2 - cycle
    };
    (offset, fill)
}

/// Renders the view; `tick` advances time-based effects (marquee, spinners and
/// the download bar), `status_green` sets the running indicator phase.
pub fn render_at(frame: &mut Frame, view: &View, tick: u64, status_green: bool, palette: Palette) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    if area.width < 60 || area.height < 16 {
        let paragraph = Paragraph::new(view.text().terminal_too_small)
            .block(titled_block(view.text().app_title))
            .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area);
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    render_header(frame, view, chunks[0], tick, status_green, palette);
    render_tabs(frame, view, chunks[1]);
    if view.overlay.is_none() {
        render_page(frame, view, chunks[2], tick, palette);
    }
    render_footer(frame, view, chunks[3], tick, palette);
    render_overlay(frame, view, area, tick, palette);
}

fn titled_block(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(format!(" {} ", title.into()))
}

fn render_header(
    frame: &mut Frame,
    view: &View,
    area: ratatui::layout::Rect,
    tick: u64,
    status_green: bool,
    palette: Palette,
) {
    let text = view.text();
    let status_text = if view.status == ProxyStatus::Running && view.uptime() != text.common_none {
        view.config
            .language
            .running_with_uptime(text.status_running, view.uptime())
    } else {
        view.status.label(text).to_string()
    };
    let status_style = match view.status {
        ProxyStatus::Running => Style::default().fg(Color::Green),
        ProxyStatus::Starting | ProxyStatus::Stopping => Style::default().fg(Color::Yellow),
        ProxyStatus::Failed => Style::default().fg(Color::Red),
        ProxyStatus::Stopped => Style::default().fg(palette.muted()),
    };
    let (status_prefix, prefix_style) = match view.status {
        ProxyStatus::Starting | ProxyStatus::Stopping => {
            (format!("{} ", spinner_frame(tick)), status_style)
        }
        ProxyStatus::Running => ("◉ ".to_string(), status_dot_style(status_green, palette)),
        ProxyStatus::Stopped | ProxyStatus::Failed => ("◉ ".to_string(), status_style),
    };
    let current = view.current_node().unwrap_or(text.no_nodes);
    let prefix = format!("{}: ", text.nodes_title);
    let middle = format!("   {}: {}", text.connections, view.connection_count);
    let api_suffix = if view.metrics_stale {
        format!(" · {}", text.msg_clash_api_unknown)
    } else {
        String::new()
    };
    let suffix = format!(
        "   {}: ↓ {} ↑ {}{api_suffix}",
        text.network,
        format_rate(view.down_speed),
        format_rate(view.up_speed)
    );
    let available = (area.width as usize).saturating_sub(4);
    let fixed = UnicodeWidthStr::width(prefix.as_str())
        + UnicodeWidthStr::width(middle.as_str())
        + UnicodeWidthStr::width(suffix.as_str());
    let current = truncate_to_width(current, available.saturating_sub(fixed).max(1));

    let left_width = UnicodeWidthStr::width(status_prefix.as_str())
        + UnicodeWidthStr::width(status_text.as_str());
    let mut first_line = vec![
        Span::styled(status_prefix, prefix_style.add_modifier(Modifier::BOLD)),
        Span::styled(status_text, status_style.add_modifier(Modifier::BOLD)),
    ];
    let (right_text, right_style) = if let Some(toast) = &view.toast {
        (toast.as_str(), Style::default().fg(Color::Yellow))
    } else {
        (text.slogan, Style::default().fg(palette.muted()))
    };
    let right_width = UnicodeWidthStr::width(right_text);
    if available >= left_width + right_width + 2 {
        first_line.push(Span::raw(" ".repeat(available - left_width - right_width)));
        first_line.push(Span::styled(right_text.to_string(), right_style));
    } else if available > left_width + 4 {
        let right_text = truncate_to_width(right_text, available - left_width - 2);
        let width = UnicodeWidthStr::width(right_text.as_str());
        first_line.push(Span::raw(" ".repeat(available - left_width - width)));
        first_line.push(Span::styled(right_text, right_style));
    }

    let lines = vec![
        Line::from(first_line),
        Line::from(format!("{prefix}{current}{middle}{suffix}")),
    ];
    let paragraph = Paragraph::new(lines)
        .block(titled_block(text.app_title))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_tabs(frame: &mut Frame, view: &View, area: ratatui::layout::Rect) {
    let titles: Vec<&str> = MAIN_PAGES
        .iter()
        .map(|page| page.title(view.text()))
        .collect();
    let tabs_area = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(1),
        ..area
    };
    let tabs = Tabs::new(titles)
        .select(view.tab_page.main_index())
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .padding(" ", " ")
        .divider("|");
    frame.render_widget(tabs, tabs_area);
}

fn render_page(
    frame: &mut Frame,
    view: &View,
    area: ratatui::layout::Rect,
    tick: u64,
    palette: Palette,
) {
    match view.page {
        Page::Dashboard => render_dashboard(frame, view, area),
        Page::Nodes => render_nodes(frame, view, area, tick),
        Page::Subscriptions => render_subscriptions(frame, view, area, tick),
        Page::Connections => render_connections(frame, view, area),
        Page::Logs => render_logs(frame, view, area, palette),
        Page::Settings => render_settings(frame, view, area, palette),
        Page::Help => render_help(frame, view, area),
    }
}

fn render_dashboard(frame: &mut Frame, view: &View, area: ratatui::layout::Rect) {
    let text = view.text();
    let inner_width = area.width.saturating_sub(4) as usize;
    let lines = dashboard_lines(view, inner_width);
    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height.max(1));
    let offset = u16::try_from(view.dashboard_scroll.min(max_scroll)).unwrap_or(u16::MAX);
    let paragraph = Paragraph::new(lines)
        .block(titled_block(text.dashboard_title))
        .scroll((offset, 0));
    frame.render_widget(paragraph, area);
}

fn binary_version_text(version: Option<&str>) -> Option<String> {
    version.map(|version| {
        if version.starts_with('v') {
            version.to_string()
        } else {
            format!("v{version}")
        }
    })
}

fn binary_status_text(view: &View) -> String {
    match view.binary_status {
        BinaryStatus::NotInstalled => view.text().setup_not_installed.to_string(),
        BinaryStatus::Installed => view.text().settings_binary_installed.to_string(),
        BinaryStatus::UpgradeAvailable => view.text().settings_binary_upgrade.to_string(),
    }
}

fn dashboard_lines(view: &View, width: usize) -> Vec<Line<'static>> {
    let text = view.text();
    let binary_version =
        binary_version_text(view.binary_version.as_deref()).unwrap_or_else(|| "—".to_string());
    let mut lines = Vec::new();
    push_key_value(
        &mut lines,
        text.listen,
        view.listen_addresses().to_string(),
        width,
    );
    push_key_value(
        &mut lines,
        text.selection_mode,
        (if view.selection_mode_auto {
            text.selection_mode_auto
        } else {
            text.selection_mode_manual
        })
        .to_string(),
        width,
    );
    push_key_value(
        &mut lines,
        text.current_node,
        view.current_node()
            .map(str::to_string)
            .unwrap_or_else(|| text.no_nodes.to_string()),
        width,
    );
    push_key_value(
        &mut lines,
        text.latency,
        view.current_delay()
            .map(|delay| format!("{delay} ms"))
            .unwrap_or_else(|| "—".to_string()),
        width,
    );
    lines.extend(multi_value_lines(text.local_ip, &view.local_ips, width));
    lines.extend(multi_value_lines(text.proxy_ip, &view.proxy_ips, width));
    push_key_value(
        &mut lines,
        text.version,
        format!("v{}", crate::version::DISPLAY),
        width,
    );
    push_key_value(&mut lines, text.binary_version, binary_version, width);
    push_key_value(
        &mut lines,
        text.subscription_summary,
        format!(
            "{} / {} {}",
            view.subscriptions.len(),
            view.node_count,
            text.nodes_title
        ),
        width,
    );
    let error_lines = key_value_wrapped(
        text.last_error,
        view.last_error
            .clone()
            .unwrap_or_else(|| text.no_error.to_string()),
        width,
    );
    let error_style = if view.last_error.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    };
    lines.extend(error_lines.into_iter().map(|line| line.style(error_style)));
    lines
}

fn push_key_value(lines: &mut Vec<Line<'static>>, label: &str, value: String, width: usize) {
    lines.extend(key_value_wrapped(label, value, width));
}

fn multi_value_lines(label: &str, values: &[String], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut values = values.iter();
    push_key_value(
        &mut lines,
        label,
        values.next().cloned().unwrap_or_else(|| "—".to_string()),
        width,
    );
    for value in values {
        push_key_value(&mut lines, "", value.clone(), width);
    }
    lines
}

/// Shared stateful table assembly for the list pages: identical column
/// spacing, highlight style, and viewport-selected state everywhere.
#[allow(clippy::too_many_arguments)]
fn render_table<const N: usize>(
    frame: &mut Frame,
    area: Rect,
    block: Block<'static>,
    header: Row<'static>,
    rows: Vec<Row<'static>>,
    columns: [Constraint; N],
    selected: usize,
    start: usize,
    end: usize,
) {
    let table = Table::new(rows, columns)
        .header(header)
        .block(block)
        .column_spacing(TABLE_COLUMN_SPACING)
        .row_highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("▸ ");
    let mut state = TableState::default();
    if start < end {
        state.select(Some(selected - start));
    }
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_nodes(frame: &mut Frame, view: &View, area: ratatui::layout::Rect, tick: u64) {
    let text = view.text();
    let block = titled_block(format!(
        "{}  {} {}/{}",
        text.nodes_title,
        text.nodes_auto_candidates,
        view.auto_candidates,
        view.nodes.len()
    ));
    if view.nodes.is_empty() {
        frame.render_widget(
            Paragraph::new(text.nodes_empty)
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let header = Row::new(vec![
        // The active-node dot shares the name column: one cell for the dot and
        // one blank cell before the name, so the label aligns with the names.
        // U+25C9 FISHEYE is East Asian Width N, so every terminal keeps it at
        // one cell; U+25CF BLACK CIRCLE is ambiguous and turns into an ellipse
        // or shifts the row in some terminals.
        Cell::from(format!("  {}", text.nodes_name)),
        Cell::from(text.nodes_source),
        Cell::from(text.nodes_protocol),
        Cell::from(text.nodes_auto),
        Cell::from(text.nodes_latency),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    let active_tag = active_node_tag(view);
    let selected = view.selected.min(view.nodes.len().saturating_sub(1));
    let (start, end) = visible_row_range(view.nodes.len(), selected, area.height.saturating_sub(3));
    let rows = view.nodes[start..end].iter().map(|node| {
        let active = active_tag == Some(node.tag.as_str());
        let latency = if view.delay_testing.contains(node.tag.as_str()) {
            "…".to_string()
        } else {
            view.node_delays
                .get(node.tag.as_str())
                .map(|delay| format!("{delay} ms"))
                .unwrap_or_else(|| "—".to_string())
        };
        let row = Row::new(vec![
            Cell::from(format!("{} {}", if active { "◉" } else { " " }, node.name)),
            Cell::from(marquee_text(&node.source, NODE_SOURCE_WIDTH as usize, tick)),
            Cell::from(node.protocol.clone()),
            Cell::from(if view.auto_tags.contains(node.tag.as_str()) {
                "✓"
            } else {
                "—"
            }),
            Cell::from(latency),
        ]);
        if active {
            row.style(Style::default().fg(Color::Cyan))
        } else {
            row
        }
    });
    render_table(
        frame,
        area,
        block,
        header,
        rows.collect(),
        NODE_COLUMNS,
        selected,
        start,
        end,
    );
}

/// Tables keep two blank cells between columns; the block padding leaves one
/// blank cell after the last column.
const TABLE_COLUMN_SPACING: u16 = 2;

/// Rows visible below the table title/header; only this window is built each
/// frame so large node/connection lists do not allocate every row.
fn visible_row_range(total: usize, selected: usize, height: u16) -> (usize, usize) {
    if total == 0 || height == 0 {
        return (0, 0);
    }
    let height = usize::from(height).min(total);
    let selected = selected.min(total - 1);
    let start = (selected + 1).saturating_sub(height).min(total - height);
    (start, start + height)
}

const NODE_SOURCE_WIDTH: u16 = 14;

const NODE_COLUMNS: [Constraint; 5] = [
    Constraint::Fill(1),
    Constraint::Length(NODE_SOURCE_WIDTH),
    Constraint::Length(11),
    Constraint::Length(4),
    Constraint::Length(8),
];

/// The single node that carries traffic, marked with a dot in the node list.
///
/// A manual selection takes precedence over the urltest result; without
/// metrics (proxy stopped or not yet polled) the pending preferred node is
/// marked instead. The previous OR-based marking could light up both the
/// manual pick and the stale urltest result at the same time.
fn active_node_tag(view: &View) -> Option<&str> {
    match view.metrics.selection.selected.as_deref() {
        Some("auto") => view.metrics.selection.auto_now.as_deref(),
        Some(tag) => Some(tag),
        None => view
            .metrics
            .selection
            .auto_now
            .as_deref()
            .or(view.preferred_node.as_deref()),
    }
}

fn render_subscriptions(frame: &mut Frame, view: &View, area: ratatui::layout::Rect, tick: u64) {
    let text = view.text();
    let block = titled_block(text.subscriptions_title);
    if view.subscriptions.is_empty() {
        frame.render_widget(
            Paragraph::new(text.subscriptions_empty)
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let (url_width, status_width) = subscription_value_widths(area);
    let header = Row::new(vec![
        Cell::from(text.subscriptions_name),
        Cell::from(text.subscriptions_url),
        Cell::from(text.subscriptions_nodes),
        Cell::from(text.subscriptions_updated),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    let selected = view
        .selected
        .min(view.subscriptions.len().saturating_sub(1));
    let (start, end) = visible_row_range(
        view.subscriptions.len(),
        selected,
        area.height.saturating_sub(3),
    );
    let rows = view.subscriptions[start..end].iter().map(|subscription| {
        let mut markers = Vec::new();
        if !subscription.enabled {
            markers.push(text.common_off.to_string());
        }
        if subscription.stale {
            markers.push(text.subscriptions_stale.to_string());
        }
        // The status column is reserved for the refresh error or the last
        // successful update time; unsupported nodes only add the (+N) count
        // to the nodes column so they never displace the timestamp.
        let status = subscription
            .error
            .clone()
            .or_else(|| {
                subscription
                    .last_updated
                    .as_deref()
                    .map(display_subscription_time)
            })
            .unwrap_or_else(|| "—".to_string());
        let status = if markers.is_empty() {
            status
        } else {
            markers.push(status);
            markers.join(" · ")
        };
        Row::new(vec![
            Cell::from(marquee_text(
                &subscription.name,
                SUBSCRIPTION_NAME_WIDTH as usize,
                tick,
            )),
            Cell::from(marquee_text(&subscription.url, url_width, tick)),
            Cell::from(format!(
                "{} (+{})",
                subscription.node_count, subscription.unsupported_count
            )),
            Cell::from(marquee_text(&status, status_width, tick)),
        ])
    });
    render_table(
        frame,
        area,
        block,
        header,
        rows.collect(),
        SUBSCRIPTION_COLUMNS,
        selected,
        start,
        end,
    );
}

const SUBSCRIPTION_NAME_WIDTH: u16 = 14;

const SUBSCRIPTION_COLUMNS: [Constraint; 4] = [
    Constraint::Length(SUBSCRIPTION_NAME_WIDTH),
    Constraint::Fill(1),
    Constraint::Length(10),
    Constraint::Length(15),
];

/// The subscription list shows month-day and time; the full timestamp stays in
/// the cache so refreshes remain unambiguous across years.
fn display_subscription_time(timestamp: &str) -> String {
    let bytes = timestamp.as_bytes();
    if bytes.len() >= 16 && bytes[4] == b'-' && bytes[7] == b'-' && bytes[10] == b' ' {
        timestamp[5..].to_string()
    } else {
        timestamp.to_string()
    }
}

const MARQUEE_STEP_TICKS: u64 = 2;
const MARQUEE_PAUSE_TICKS: u64 = 8;

fn subscription_value_widths(area: Rect) -> (usize, usize) {
    // Mirrors the table layout: borders + horizontal padding 2, highlight symbol "▸ ".
    let table_width = area.width.saturating_sub(4);
    let columns_width = table_width.saturating_sub(2);
    let columns = Layout::horizontal(SUBSCRIPTION_COLUMNS)
        .spacing(TABLE_COLUMN_SPACING)
        .split(Rect::new(0, 0, columns_width, 1));
    (columns[1].width as usize, columns[3].width as usize)
}

fn marquee_text(text: &str, width: usize, tick: u64) -> String {
    if width == 0 {
        return String::new();
    }
    let text_width = UnicodeWidthStr::width(text);
    if text_width <= width {
        return text.to_string();
    }
    marquee_slice(text, width, marquee_offset(tick, text_width - width))
}

fn marquee_offset(tick: u64, overflow: usize) -> usize {
    if overflow == 0 {
        return 0;
    }
    let travel = overflow as u64;
    let scroll = travel * MARQUEE_STEP_TICKS;
    let cycle = 2 * (MARQUEE_PAUSE_TICKS + scroll);
    let phase = tick % cycle;
    if phase < MARQUEE_PAUSE_TICKS {
        0
    } else if phase < MARQUEE_PAUSE_TICKS + scroll {
        ((phase - MARQUEE_PAUSE_TICKS) / MARQUEE_STEP_TICKS).min(travel) as usize
    } else if phase < 2 * MARQUEE_PAUSE_TICKS + scroll {
        overflow
    } else {
        let back = (phase - 2 * MARQUEE_PAUSE_TICKS - scroll) / MARQUEE_STEP_TICKS;
        overflow.saturating_sub(back as usize)
    }
}

fn marquee_slice(text: &str, width: usize, offset: usize) -> String {
    let mut out = String::new();
    let mut position = 0usize;
    let mut out_width = 0usize;
    for ch in text.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if position + char_width <= offset {
            position += char_width;
            continue;
        }
        if position < offset || out_width + char_width > width {
            position += char_width;
            continue;
        }
        out.push(ch);
        position += char_width;
        out_width += char_width;
    }
    out
}

fn render_connections(frame: &mut Frame, view: &View, area: ratatui::layout::Rect) {
    let text = view.text();
    let block = titled_block(text.connections_title);
    if view.metrics.connections.is_empty() {
        frame.render_widget(
            Paragraph::new(text.connections_empty)
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let header = Row::new(vec![
        Cell::from(text.connections_dest),
        Cell::from(text.connections_network),
        Cell::from(text.connections_traffic),
        Cell::from(text.connections_duration),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    let selected = view
        .selected
        .min(view.metrics.connections.len().saturating_sub(1));
    let (start, end) = visible_row_range(
        view.metrics.connections.len(),
        selected,
        area.height.saturating_sub(3),
    );
    let rows = view.metrics.connections[start..end]
        .iter()
        .map(|connection| {
            Row::new(vec![
                Cell::from(connection_target(connection)),
                Cell::from(connection.network.clone()),
                Cell::from(format!(
                    "↓ {} ↑ {}",
                    format_bytes(connection.download),
                    format_bytes(connection.upload)
                )),
                Cell::from(format_duration(connection.duration_secs)),
            ])
        });
    render_table(
        frame,
        area,
        block,
        header,
        rows.collect(),
        CONNECTION_COLUMNS,
        selected,
        start,
        end,
    );
}

const CONNECTION_COLUMNS: [Constraint; 4] = [
    Constraint::Fill(1),
    Constraint::Length(7),
    Constraint::Length(21),
    Constraint::Length(9),
];

fn connection_target(connection: &crate::singbox::clash::ConnectionInfo) -> String {
    connection.target().unwrap_or_else(|| "—".to_string())
}

/// Connection age: `MM:SS` below one hour, `H:MM:SS` from one hour on.
fn format_duration(seconds: Option<u64>) -> String {
    let Some(total) = seconds else {
        return "—".to_string();
    };
    let hours = total / 3600;
    let minutes = (total / 60) % 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn log_level_style(level: LogLevel, palette: Palette) -> Style {
    match level {
        LogLevel::Error => Style::default().fg(Color::Red),
        LogLevel::Warn => Style::default().fg(Color::Yellow),
        LogLevel::Info => Style::default(),
        LogLevel::Debug | LogLevel::Trace => Style::default().fg(palette.muted()),
    }
}

fn render_logs(frame: &mut Frame, view: &View, area: ratatui::layout::Rect, palette: Palette) {
    let text = view.text();
    let follow = if view.log_follow {
        text.common_on
    } else {
        text.common_off
    };
    let title = format!(
        "{}  [{}] [{}]  {}:{}",
        text.logs_title,
        view.log_source.label(),
        view.log_filter.label(),
        text.logs_follow,
        follow
    );
    let block = titled_block(title);
    if view.logs.is_empty() {
        let empty = if view.log_source == LogSource::All {
            text.logs_empty
        } else {
            text.logs_empty_source
        };
        frame.render_widget(Paragraph::new(empty).block(block), area);
        return;
    }
    let selected = view.selected.min(view.logs.len().saturating_sub(1));
    let (start, end) = visible_row_range(view.logs.len(), selected, area.height.saturating_sub(2));
    let lines: Vec<ListItem> = view.logs[start..end]
        .iter()
        .map(|line| {
            ListItem::new(Line::from(Span::styled(
                line.display(view.log_timestamp),
                log_level_style(line.level, palette),
            )))
        })
        .collect();
    let mut state = ListState::default();
    if start < end {
        state.select(Some(selected - start));
    }
    frame.render_stateful_widget(
        List::new(lines)
            .block(block)
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan)),
        area,
        &mut state,
    );
}

fn render_settings(frame: &mut Frame, view: &View, area: ratatui::layout::Rect, palette: Palette) {
    let text = view.text();
    let block = titled_block(format!(
        "{}  {}",
        text.settings_title, text.settings_key_hints
    ));
    let rows: Vec<(&'static str, String, bool)> = settings_items(view.config.allow_lan)
        .iter()
        .map(|item| setting_content(view, *item))
        .collect();
    let label_width = rows
        .iter()
        .map(|(label, _, _)| UnicodeWidthStr::width(*label))
        .max()
        .unwrap_or(0)
        .max(16)
        + 2;
    let inner_width = area.width.saturating_sub(4) as usize;
    let value_width = inner_width.saturating_sub(2 + label_width).max(1);
    let item_count = rows.len();
    let items: Vec<ListItem> = rows
        .into_iter()
        .enumerate()
        .map(|(index, (label, value, read_only))| {
            ListItem::new(settings_lines(
                index == view.selected,
                label,
                value,
                read_only.then_some(text.settings_read_only),
                label_width,
                value_width,
                palette,
            ))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(view.selected.min(item_count.saturating_sub(1))));
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));
    frame.render_stateful_widget(list, area, &mut state);
}

fn setting_content(view: &View, item: SettingsItem) -> (&'static str, String, bool) {
    let text = view.text();
    let on_off = |value: bool| {
        if value {
            text.common_on
        } else {
            text.common_off
        }
    };
    let (label, value) = match item {
        SettingsItem::Language => (text.settings_language, view.language_label().to_string()),
        SettingsItem::Port => (text.settings_port, view.config.port.to_string()),
        SettingsItem::AllowLan => (
            text.settings_allow_lan,
            on_off(view.config.allow_lan).to_string(),
        ),
        SettingsItem::LanAuth => {
            let username = if view.config.lan_username.is_empty() {
                text.settings_unset.to_string()
            } else {
                view.config.lan_username.clone()
            };
            let password = if view.lan_password_set {
                text.settings_set.to_string()
            } else {
                text.settings_unset.to_string()
            };
            (text.settings_lan_auth, format!("{username} / {password}"))
        }
        SettingsItem::DnsServers => (
            text.settings_dns_servers,
            view.config.dns_servers.join(", "),
        ),
        SettingsItem::TestUrl => (text.settings_test_url, view.config.test_url.clone()),
        SettingsItem::KeywordsInclude => (
            text.settings_keywords_include,
            if view.config.keywords_include.is_empty() {
                text.settings_keywords_all.to_string()
            } else {
                view.config.keywords_include.join(", ")
            },
        ),
        SettingsItem::KeywordsExclude => (
            text.settings_keywords_exclude,
            if view.config.keywords_exclude.is_empty() {
                text.settings_keywords_none.to_string()
            } else {
                view.config.keywords_exclude.join(", ")
            },
        ),
        SettingsItem::AppLogLevel => (
            text.settings_app_log_level,
            view.config.app_log_level.label().to_string(),
        ),
        SettingsItem::BinaryInfo => (text.settings_binary_info, binary_status_text(view)),
        SettingsItem::ConfigPath => (text.settings_config_path, view.config_path.clone()),
        SettingsItem::LogFile => (text.settings_log_file, view.log_file.display().to_string()),
    };
    (label, value, item.read_only())
}

fn settings_lines(
    selected: bool,
    label: &str,
    value: String,
    read_only_marker: Option<&str>,
    label_width: usize,
    value_width: usize,
    palette: Palette,
) -> Vec<Line<'static>> {
    let padding = " ".repeat(label_width.saturating_sub(UnicodeWidthStr::width(label)));
    let indent = " ".repeat(2 + label_width);
    let (row_marker_style, label_style, value_style) = if selected {
        (
            Style::default().fg(Color::Black),
            Style::default().fg(Color::Black),
            Style::default().fg(Color::Black),
        )
    } else {
        (
            Style::default().fg(Color::Cyan),
            Style::default(),
            Style::default(),
        )
    };
    let ro_marker_style = if selected {
        Style::default().fg(Color::Black)
    } else {
        Style::default().fg(palette.muted())
    };
    let marker_width = read_only_marker
        .map(|marker| 1 + UnicodeWidthStr::width(marker))
        .unwrap_or(0);
    let wrap_width = value_width.saturating_sub(marker_width).max(1);
    let chunks = wrap_display_text(&value, wrap_width);
    let last_index = chunks.len().saturating_sub(1);
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let mut spans = if index == 0 {
                vec![
                    Span::styled(if selected { "▸ " } else { "  " }, row_marker_style),
                    Span::styled(format!("{label}{padding}"), label_style),
                ]
            } else {
                vec![Span::raw(indent.clone())]
            };
            spans.push(Span::styled(chunk, value_style));
            if index == last_index {
                if let Some(marker) = read_only_marker {
                    spans.push(Span::styled(format!(" {marker}"), ro_marker_style));
                }
            }
            Line::from(spans)
        })
        .collect()
}

fn setup_lines(
    label: &str,
    value: String,
    label_width: usize,
    value_width: usize,
) -> Vec<Line<'static>> {
    let padding = " ".repeat(label_width.saturating_sub(UnicodeWidthStr::width(label)));
    let indent = " ".repeat(label_width);
    wrap_display_text(&value, value_width)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            if index == 0 {
                Line::from(vec![
                    Span::raw(format!("{label}{padding}")),
                    Span::raw(chunk),
                ])
            } else {
                Line::from(vec![Span::raw(indent.clone()), Span::raw(chunk)])
            }
        })
        .collect()
}

/// Busy status inside the sing-box popup. Download adds an indeterminate bar
/// because the binary download exposes no byte-level progress.
fn animated_status_line(
    label: &str,
    tick: u64,
    width: usize,
    palette: Palette,
    progress: bool,
) -> Line<'static> {
    let prefix = format!("{} {label}", spinner_frame(tick));
    let prefix_width = UnicodeWidthStr::width(prefix.as_str());
    let mut spans = vec![Span::styled(prefix, Style::default().fg(Color::Yellow))];
    if progress && prefix_width + 2 + PROGRESS_BAR_WIDTH + 2 <= width {
        let muted = Style::default().fg(palette.muted());
        let filled = Style::default().fg(Color::Yellow);
        let (offset, fill) = progress_fill(tick, PROGRESS_BAR_WIDTH);
        spans.push(Span::raw("  "));
        spans.push(Span::styled("[", muted));
        if offset > 0 {
            spans.push(Span::styled("░".repeat(offset), muted));
        }
        spans.push(Span::styled("█".repeat(fill), filled));
        let tail = PROGRESS_BAR_WIDTH.saturating_sub(offset + fill);
        if tail > 0 {
            spans.push(Span::styled("░".repeat(tail), muted));
        }
        spans.push(Span::styled("]", muted));
    }
    Line::from(spans)
}

fn setup_content_lines(
    view: &View,
    inner_width: usize,
    selected: usize,
    tick: u64,
    palette: Palette,
) -> (Vec<Line<'static>>, usize) {
    let text = view.text();
    let installed = view.binary_status != BinaryStatus::NotInstalled;
    let mut rows: Vec<(&'static str, String)> = Vec::new();
    if installed {
        let current =
            binary_version_text(view.binary_version.as_deref()).unwrap_or_else(|| "—".to_string());
        rows.push((text.setup_current_version, current));
        let verified = match view.binary_verified {
            Some(true) => text.setup_verified_ok,
            Some(false) => text.setup_unverified,
            None => text.settings_unset,
        };
        rows.push((text.setup_verified, verified.to_string()));
    }
    if let Some(latest) = binary_version_text(view.latest_binary_version.as_deref()) {
        rows.push((text.setup_latest_version, latest));
    }
    if view.previous_binary_available {
        let previous = binary_version_text(view.previous_binary_version.as_deref())
            .unwrap_or_else(|| "—".to_string());
        rows.push((text.setup_previous_version, previous));
    }
    rows.push((text.setup_status, binary_status_text(view)));
    rows.push((
        text.settings_singbox_log_level,
        view.config.singbox_log_level.label().to_string(),
    ));
    rows.push((text.settings_binary, view.binary_path.clone()));
    rows.push((
        text.setup_official_url,
        "https://github.com/SagerNet/sing-box/releases".to_string(),
    ));
    rows.push((text.setup_inbox_path, view.inbox_path.clone()));

    let label_width = rows
        .iter()
        .map(|(label, _)| UnicodeWidthStr::width(*label))
        .max()
        .unwrap_or(0)
        + 2;
    let value_width = inner_width.saturating_sub(label_width).max(1);
    let mut lines = Vec::new();
    for (label, value) in rows {
        lines.extend(setup_lines(label, value, label_width, value_width));
    }
    let muted = Style::default().fg(palette.muted());
    lines.push(Line::from(Span::styled(text.setup_formats, muted)));
    lines.push(Line::from(Span::styled(text.setup_checksum, muted)));
    if view.setup.installing {
        lines.push(animated_status_line(
            text.setup_installing,
            tick,
            inner_width,
            palette,
            false,
        ));
    } else if view.setup.rollback_in_flight {
        lines.push(animated_status_line(
            text.setup_switching,
            tick,
            inner_width,
            palette,
            false,
        ));
    } else if view.setup.downloading {
        lines.push(animated_status_line(
            text.setup_downloading,
            tick,
            inner_width,
            palette,
            true,
        ));
    } else {
        lines.push(Line::from(Span::styled(
            text.setup_install_hint,
            Style::default().fg(Color::Yellow),
        )));
    }
    if let Some(message) = &view.setup.message {
        lines.push(Line::from(Span::styled(
            message.clone(),
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(""));
    let candidate_start = lines.len();
    if view.setup.candidates.is_empty() {
        lines.push(Line::from(text.setup_no_candidates));
    } else {
        let selected_style = Style::default().fg(Color::Black).bg(Color::Cyan);
        for (index, candidate) in view.setup.candidates.iter().enumerate() {
            let is_selected = index == selected;
            let suffix = if candidate.has_checksum {
                " [sha256]"
            } else {
                " [!]"
            };
            let suffix_width = UnicodeWidthStr::width(suffix);
            let label_available = inner_width.saturating_sub(2 + suffix_width).max(1);
            let label = truncate_to_width(&candidate.label, label_available);
            let mut spans = Vec::new();
            if is_selected {
                let content_width = 2 + UnicodeWidthStr::width(label.as_str()) + suffix_width;
                spans.push(Span::styled("▸ ", selected_style));
                spans.push(Span::styled(label, selected_style));
                spans.push(Span::styled(suffix, selected_style));
                let pad = inner_width.saturating_sub(content_width);
                if pad > 0 {
                    spans.push(Span::styled(" ".repeat(pad), selected_style));
                }
            } else {
                let suffix_style = if candidate.has_checksum {
                    muted
                } else {
                    Style::default().fg(Color::Yellow)
                };
                spans.push(Span::raw("  "));
                spans.push(Span::raw(label));
                spans.push(Span::styled(suffix, suffix_style));
            }
            lines.push(Line::from(spans));
        }
    }
    (lines, candidate_start)
}

fn render_setup_popup(
    frame: &mut Frame,
    view: &View,
    selected: usize,
    area: ratatui::layout::Rect,
    tick: u64,
    palette: Palette,
) {
    let text = view.text();
    let width = popup_width(area, 12, 72, 100);
    let inner_width = width.saturating_sub(4) as usize;
    let (lines, candidate_start) = setup_content_lines(view, inner_width, selected, tick, palette);
    let height = popup_height(area, &lines);
    let inner_height = height.saturating_sub(2) as usize;
    let selected_line = candidate_start.saturating_add(selected);
    let offset = if view.setup.candidates.is_empty() {
        0
    } else {
        selected_line.saturating_sub(inner_height.saturating_sub(1))
    };
    let popup = centered_rect(area, width, height);
    frame.render_widget(Clear, popup);
    let title = format!("{}  {}", text.setup_title, text.setup_key_hints);
    let paragraph = Paragraph::new(lines)
        .block(titled_block(title))
        .wrap(Wrap { trim: false })
        .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0));
    frame.render_widget(paragraph, popup);
}

fn help_content_lines(view: &View, width: usize) -> Vec<Line<'static>> {
    let text = view.text();
    let mut lines = Vec::new();
    push_help_line(
        &mut lines,
        &format!("{}: DFRelay v{}", text.version, crate::version::DISPLAY),
        Style::default(),
        width,
    );
    lines.push(Line::from(""));
    for (index, section) in text.help_sections.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        push_help_line(
            &mut lines,
            section.title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            width,
        );
        for line in section.lines {
            push_help_line(&mut lines, &format!("  {line}"), Style::default(), width);
        }
    }
    lines
}

fn push_help_line(lines: &mut Vec<Line<'static>>, text: &str, style: Style, width: usize) {
    for chunk in wrap_help_text(text, width) {
        lines.push(Line::from(Span::styled(chunk, style)));
    }
}

fn help_page_max_scroll(view: &View, width: u16, page_height: u16) -> usize {
    let visible = page_height.saturating_sub(2) as usize;
    let inner_width = width.saturating_sub(4).max(1) as usize;
    let total = help_content_lines(view, inner_width).len();
    total.saturating_sub(visible.max(1))
}

pub fn help_max_scroll(view: &View, width: u16, height: u16) -> usize {
    if view.page != Page::Help || width < 60 || height < 16 {
        return 0;
    }
    let page_height = height.saturating_sub(HEADER_HEIGHT + 2);
    help_page_max_scroll(view, width, page_height)
}

pub fn dashboard_max_scroll(view: &View, width: u16, height: u16) -> usize {
    if view.page != Page::Dashboard || width < 60 || height < 16 {
        return 0;
    }
    let page_height = height.saturating_sub(HEADER_HEIGHT + 2);
    let inner_height = page_height.saturating_sub(2) as usize;
    let inner_width = width.saturating_sub(4) as usize;
    dashboard_lines(view, inner_width)
        .len()
        .saturating_sub(inner_height.max(1))
}

fn render_help(frame: &mut Frame, view: &View, area: ratatui::layout::Rect) {
    let text = view.text();
    let inner_width = area.width.saturating_sub(4).max(1) as usize;
    let lines = help_content_lines(view, inner_width);
    let max_scroll = help_page_max_scroll(view, area.width, area.height);
    let offset = view.help_scroll.min(max_scroll);
    let paragraph = Paragraph::new(lines)
        .block(titled_block(text.help_title))
        .wrap(Wrap { trim: false })
        .scroll((offset as u16, 0));
    frame.render_widget(paragraph, area);
}

struct InputField<'a> {
    marker: &'static str,
    label: &'static str,
    label_width: usize,
    input: &'a LineInput,
    focused: bool,
    label_style: Style,
}

fn input_lines(field: InputField<'_>, width: usize) -> Vec<Line<'static>> {
    let InputField {
        marker,
        label,
        label_width,
        input,
        focused,
        label_style,
    } = field;
    let padding = " ".repeat(label_width.saturating_sub(UnicodeWidthStr::width(label)));
    let prefix_width = UnicodeWidthStr::width(marker) + label_width + UnicodeWidthStr::width(": ");
    let value_width = width.saturating_sub(prefix_width).saturating_sub(1).max(1);
    let chars: Vec<char> = input.value.chars().collect();
    let cursor = input.cursor.min(chars.len());
    let mut chunks: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    let mut current_width = 0;
    for (index, ch) in chars.iter().enumerate() {
        let char_width = UnicodeWidthChar::width(*ch).unwrap_or(0);
        if index > start && current_width + char_width > value_width {
            chunks.push((start, index));
            start = index;
            current_width = 0;
        }
        current_width += char_width;
    }
    chunks.push((start, chars.len()));

    let marker_style = field_marker_style(focused);
    let cursor_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut caret_placed = false;
    let mut lines = Vec::new();
    for (line_index, (start, end)) in chunks.iter().copied().enumerate() {
        let mut spans = Vec::new();
        if line_index == 0 {
            if !marker.is_empty() {
                spans.push(Span::styled(marker, marker_style));
            }
            spans.push(Span::styled(format!("{label}{padding}"), label_style));
            spans.push(Span::raw(": "));
        } else {
            spans.push(Span::raw(" ".repeat(prefix_width)));
        }
        let mut text = String::new();
        for (index, ch) in chars.iter().enumerate().take(end).skip(start) {
            if focused && !caret_placed && index == cursor {
                if !text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut text), Style::default()));
                }
                spans.push(Span::styled("▏", cursor_style));
                caret_placed = true;
            }
            text.push(*ch);
        }
        if focused && !caret_placed && cursor == end {
            if !text.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut text), Style::default()));
            }
            spans.push(Span::styled("▏", cursor_style));
            caret_placed = true;
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, Style::default()));
        }
        if spans.is_empty() {
            spans.push(Span::raw(""));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn popup_width(area: Rect, margin: u16, min: u16, max: u16) -> u16 {
    area.width
        .saturating_sub(margin)
        .clamp(min, max)
        .min(area.width)
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn popup_paragraph(title: &str, lines: Vec<Line<'static>>) -> Paragraph<'static> {
    Paragraph::new(lines).block(titled_block(title))
}

/// Total popup height for the given lines, including the border. All popup
/// content is pre-wrapped to the popup width, so this is exact.
fn popup_height(area: Rect, lines: &[Line<'static>]) -> u16 {
    let height = lines.len().saturating_add(2);
    u16::try_from(height).unwrap_or(u16::MAX).min(area.height)
}

fn render_popup(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>, width: u16) {
    let height = popup_height(area, &lines);
    let popup = centered_rect(area, width, height);
    frame.render_widget(Clear, popup);
    frame.render_widget(popup_paragraph(title, lines), popup);
}

fn active_label_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn inactive_label_style(palette: Palette) -> Style {
    Style::default().fg(palette.muted())
}

fn field_label_style(focused: bool, palette: Palette) -> Style {
    if focused {
        active_label_style()
    } else {
        inactive_label_style(palette)
    }
}

/// Multi-field edit forms mark the active row; single-field forms rely on the
/// cursor only.
fn field_marker(focused: bool) -> &'static str {
    if focused {
        "▸ "
    } else {
        "  "
    }
}

fn field_marker_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn push_edit_feedback(
    lines: &mut Vec<Line<'static>>,
    error: Option<&str>,
    hint: &str,
    inner_width: usize,
    palette: Palette,
) {
    lines.push(Line::from(""));
    if let Some(error) = error {
        for chunk in wrap_display_text(error, inner_width) {
            lines.push(Line::from(Span::styled(
                chunk,
                Style::default().fg(Color::Red),
            )));
        }
    }
    for chunk in wrap_display_text(hint, inner_width) {
        lines.push(Line::from(Span::styled(
            chunk,
            Style::default().fg(palette.muted()),
        )));
    }
}

fn render_overlay(frame: &mut Frame, view: &View, area: Rect, tick: u64, palette: Palette) {
    let Some(overlay) = &view.overlay else {
        return;
    };
    match overlay {
        Overlay::Edit(form) => render_edit_form(frame, view.text(), form, area, palette),
        Overlay::Setup { selected } => {
            render_setup_popup(frame, view, *selected, area, tick, palette)
        }
        Overlay::LogDetail { line, scroll } => {
            render_log_detail(frame, view.text(), line, *scroll, area, palette)
        }
    }
}

fn render_edit_form(frame: &mut Frame, text: &Text, form: &EditForm, area: Rect, palette: Palette) {
    let width = popup_width(area, 12, 44, 88);
    let inner_width = width.saturating_sub(4) as usize;
    let mut lines = match form {
        EditForm::Subscription(form) => subscription_form_lines(text, form, inner_width, palette),
        EditForm::Keyword(form) => keyword_form_lines(text, form, inner_width),
        EditForm::Settings(form) => settings_form_lines(text, form, inner_width, palette),
    };
    let error = match form {
        EditForm::Subscription(form) => form.error.as_deref(),
        EditForm::Keyword(form) => form.error.as_deref(),
        EditForm::Settings(form) => form.error.as_deref(),
    };
    push_edit_feedback(&mut lines, error, form.hint(text), inner_width, palette);
    render_popup(frame, area, form.title(text), lines, width);
}

fn subscription_form_lines(
    text: &Text,
    form: &SubscriptionForm,
    inner_width: usize,
    palette: Palette,
) -> Vec<Line<'static>> {
    let label_width = [
        text.subscription_form_name,
        text.subscription_form_url,
        text.subscription_form_enabled,
    ]
    .iter()
    .map(|label| UnicodeWidthStr::width(*label))
    .max()
    .unwrap_or(0);

    let mut lines = input_lines(
        InputField {
            marker: field_marker(form.focus == 0),
            label: text.subscription_form_name,
            label_width,
            input: &form.name,
            focused: form.focus == 0,
            label_style: field_label_style(form.focus == 0, palette),
        },
        inner_width,
    );
    lines.extend(input_lines(
        InputField {
            marker: field_marker(form.focus == 1),
            label: text.subscription_form_url,
            label_width,
            input: &form.url,
            focused: form.focus == 1,
            label_style: field_label_style(form.focus == 1, palette),
        },
        inner_width,
    ));
    let enabled_label = text.subscription_form_enabled;
    let enabled_padding =
        " ".repeat(label_width.saturating_sub(UnicodeWidthStr::width(enabled_label)));
    lines.push(Line::from(vec![
        Span::styled(
            field_marker(form.focus == 2),
            field_marker_style(form.focus == 2),
        ),
        Span::styled(
            format!("{enabled_label}{enabled_padding}"),
            field_label_style(form.focus == 2, palette),
        ),
        Span::raw(": "),
        Span::raw(if form.enabled { "[✓]" } else { "[ ]" }),
        Span::raw("  Space"),
    ]));
    lines
}

fn keyword_form_lines(text: &Text, form: &KeywordForm, inner_width: usize) -> Vec<Line<'static>> {
    let title = form.title(text);
    input_lines(
        InputField {
            marker: "",
            label: title,
            label_width: UnicodeWidthStr::width(title),
            input: &form.value,
            focused: true,
            label_style: active_label_style(),
        },
        inner_width,
    )
}

fn settings_form_lines(
    text: &Text,
    form: &SettingsForm,
    inner_width: usize,
    palette: Palette,
) -> Vec<Line<'static>> {
    let label_width = UnicodeWidthStr::width(text.settings_lan_username)
        .max(UnicodeWidthStr::width(text.settings_lan_password));
    let username_focused = form.active == SettingsField::LanUsername;
    let password_focused = form.active == SettingsField::LanPassword;
    let mut lines = input_lines(
        InputField {
            marker: field_marker(username_focused),
            label: text.settings_lan_username,
            label_width,
            input: &form.username,
            focused: username_focused,
            label_style: field_label_style(username_focused, palette),
        },
        inner_width,
    );
    lines.extend(input_lines(
        InputField {
            marker: field_marker(password_focused),
            label: text.settings_lan_password,
            label_width,
            input: &form.password,
            focused: password_focused,
            label_style: field_label_style(password_focused, palette),
        },
        inner_width,
    ));
    lines
}

fn log_detail_lines(line: &LogLine, inner_width: usize, palette: Palette) -> Vec<Line<'static>> {
    let level_style = log_level_style(line.level, palette);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(line.level.label(), level_style),
            Span::raw(format!("  [{}]", line.component.label())),
        ]),
        Line::from(Span::styled(
            line.timestamp.clone(),
            Style::default().fg(palette.muted()),
        )),
        Line::from(""),
    ];
    for chunk in wrap_display_text(&line.message, inner_width) {
        lines.push(Line::from(chunk));
    }
    lines
}

fn log_detail_hint_lines(text: &Text, inner_width: usize, palette: Palette) -> Vec<Line<'static>> {
    wrap_display_text(text.logs_detail_hint, inner_width)
        .into_iter()
        .map(|chunk| Line::from(Span::styled(chunk, Style::default().fg(palette.muted()))))
        .collect()
}

fn log_detail_area(area: Rect) -> (u16, u16) {
    let width = popup_width(area, 10, 40, 100);
    let height = area.height.saturating_sub(4).clamp(8, 14).min(area.height);
    (width, height)
}

fn log_detail_max_scroll_for(text: &Text, line: &LogLine, area: Rect, palette: Palette) -> usize {
    let (width, height) = log_detail_area(area);
    let inner_width = width.saturating_sub(4) as usize;
    let content = log_detail_lines(line, inner_width, palette).len();
    let hint = log_detail_hint_lines(text, inner_width, palette).len();
    let visible = (height.saturating_sub(2) as usize)
        .saturating_sub(hint)
        .max(1);
    content.saturating_sub(visible)
}

pub fn log_detail_max_scroll(view: &View, width: u16, height: u16, palette: Palette) -> usize {
    if width < 60 || height < 16 {
        return 0;
    }
    let Some((line, _)) = view.overlay.as_ref().and_then(Overlay::log_detail) else {
        return 0;
    };
    log_detail_max_scroll_for(view.text(), line, Rect::new(0, 0, width, height), palette)
}

fn render_log_detail(
    frame: &mut Frame,
    text: &Text,
    line: &LogLine,
    scroll: usize,
    area: Rect,
    palette: Palette,
) {
    let (width, height) = log_detail_area(area);
    let max_scroll = log_detail_max_scroll_for(text, line, area, palette);
    let offset = scroll.min(max_scroll);
    let popup = centered_rect(area, width, height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new("").block(titled_block(text.logs_detail_title)),
        popup,
    );

    let inner = Rect {
        x: popup.x.saturating_add(2),
        y: popup.y.saturating_add(1),
        width: popup.width.saturating_sub(4),
        height: popup.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let inner_width = inner.width as usize;
    let hint_lines = log_detail_hint_lines(text, inner_width, palette);
    let hint_height = u16::try_from(hint_lines.len())
        .unwrap_or(u16::MAX)
        .min(inner.height);
    let content_height = inner.height.saturating_sub(hint_height);
    let content_area = Rect {
        height: content_height,
        ..inner
    };
    let hint_area = Rect {
        y: inner.y.saturating_add(content_height),
        height: hint_height,
        ..inner
    };
    let content = Paragraph::new(log_detail_lines(line, inner_width, palette))
        .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0));
    frame.render_widget(content, content_area);
    frame.render_widget(Paragraph::new(hint_lines), hint_area);
}

fn render_footer(
    frame: &mut Frame,
    view: &View,
    area: ratatui::layout::Rect,
    tick: u64,
    palette: Palette,
) {
    let text = view.text();
    // Keep two blank cells free at both edges so hints never touch the border.
    let area = Rect {
        x: area.x.saturating_add(2),
        width: area.width.saturating_sub(4),
        ..area
    };
    let mut spans = if view.restart_in_progress {
        vec![Span::styled(
            format!("{} {}", spinner_frame(tick), text.msg_restarting),
            Style::default().fg(Color::Yellow),
        )]
    } else if view.restart_confirm {
        vec![Span::styled(
            text.msg_restart_confirm,
            Style::default().fg(Color::Yellow),
        )]
    } else if view.rollback_confirm {
        vec![Span::styled(
            text.msg_rollback_confirm,
            Style::default().fg(Color::Yellow),
        )]
    } else if view.setup.pending_install.is_some() {
        vec![Span::styled(
            text.setup_confirm_unverified,
            Style::default().fg(Color::Yellow),
        )]
    } else if view.subscription_delete {
        vec![Span::styled(
            text.msg_subscription_delete_confirm,
            Style::default().fg(Color::Yellow),
        )]
    } else if view.overlay.is_some() {
        // Modal popups carry their own key hints; global hints would not apply.
        Vec::new()
    } else {
        footer_hint_spans(text, palette)
    };
    if view.page == Page::Settings && view.config.allow_lan {
        let (message, color) = if view.lan_auth_configured() {
            (text.settings_lan_auth_short, Color::Yellow)
        } else {
            (text.settings_lan_warning_short, Color::Red)
        };
        let left_width: usize = spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        let right_width = UnicodeWidthStr::width(message);
        let total = area.width as usize;
        if left_width + right_width + 2 <= total {
            spans.push(Span::raw(" ".repeat(total - left_width - right_width)));
            spans.push(Span::styled(message, Style::default().fg(color)));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn footer_hint_spans(text: &Text, palette: Palette) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, hint) in text.help_footer.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(hint.key, Style::default()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            hint.label,
            Style::default().fg(palette.muted()),
        ));
    }
    spans
}

fn truncate_to_width(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + char_width >= max {
            out.push('…');
            break;
        }
        out.push(ch);
        width += char_width;
    }
    out
}

// Widest dashboard label is "Subscriptions" (13 columns); keep at least two
// blank columns between the label and its value in every language.
const DASHBOARD_LABEL_WIDTH: usize = 15;

fn key_value_wrapped(label: &str, value: String, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(DASHBOARD_LABEL_WIDTH).max(1);
    setup_lines(label, value, DASHBOARD_LABEL_WIDTH, value_width)
}

#[cfg(test)]
mod tests;
