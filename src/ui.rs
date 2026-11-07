use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Padding, Paragraph, Row, Table,
    TableState, Tabs, Wrap,
};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    settings_items, EditForm, KeywordForm, Overlay, Page, ProxyStatus, SettingsField, SettingsForm,
    SettingsItem, SubscriptionForm, MAIN_PAGES,
};
use crate::config::UpdatePolicy;
use crate::i18n::Text;
use crate::logging::{LogLevel, LogLine, LogSource};
use crate::theme::Palette;
use crate::view::{BinaryStatus, View};

const HEADER_HEIGHT: u16 = 4;

mod format;
use format::*;

#[cfg(test)]
pub fn render(frame: &mut Frame, view: &View) {
    render_at(frame, view, 0, Palette::Dark);
}

/// Renders the view; `tick` advances time-based marquee scrolling (100 ms units).
pub fn render_at(frame: &mut Frame, view: &View, tick: u64, palette: Palette) {
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

    render_header(frame, view, chunks[0], palette);
    render_tabs(frame, view, chunks[1]);
    if view.overlay.is_none() {
        render_page(frame, view, chunks[2], tick, palette);
    }
    render_footer(frame, view, chunks[3], palette);
    render_overlay(frame, view, area, palette);
}

fn titled_block(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(format!(" {} ", title.into()))
}

fn render_header(frame: &mut Frame, view: &View, area: ratatui::layout::Rect, palette: Palette) {
    let text = view.text();
    let status_display = if view.status == ProxyStatus::Running && view.uptime() != text.common_none
    {
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
    let current = view.current_node().unwrap_or(text.no_nodes);
    let prefix = format!("{}: ", text.nodes_title);
    let middle = format!(
        "   {}: {}",
        text.connections,
        view.metrics.connections.len()
    );
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

    let left_width = UnicodeWidthStr::width(status_display.as_str());
    let mut first_line = vec![Span::styled(
        status_display,
        status_style.add_modifier(Modifier::BOLD),
    )];
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
            view.nodes.len(),
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
        SettingsItem::AppUpdate => {
            let policy = match view.config.app_update {
                UpdatePolicy::Off => text.app_update_off,
                UpdatePolicy::Notify => text.app_update_notify,
                UpdatePolicy::Auto => text.app_update_auto,
            };
            let value = match &view.pending_app_update {
                Some(version) => format!("{policy} · v{version}"),
                None => policy.to_string(),
            };
            (text.settings_app_update, value)
        }
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

fn setup_content_lines(
    view: &View,
    inner_width: usize,
    selected: usize,
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
        lines.push(Line::from(Span::styled(
            text.setup_installing,
            Style::default().fg(Color::Yellow),
        )));
    } else if view.setup.rollback_in_flight {
        lines.push(Line::from(Span::styled(
            text.setup_switching,
            Style::default().fg(Color::Yellow),
        )));
    } else if view.setup.downloading {
        lines.push(Line::from(Span::styled(
            text.setup_downloading,
            Style::default().fg(Color::Yellow),
        )));
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
    palette: Palette,
) {
    let text = view.text();
    let width = popup_width(area, 12, 72, 100);
    let inner_width = width.saturating_sub(4) as usize;
    let (lines, candidate_start) = setup_content_lines(view, inner_width, selected, palette);
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
    if view.page != crate::app::Page::Help || width < 60 || height < 16 {
        return 0;
    }
    let page_height = height.saturating_sub(HEADER_HEIGHT + 2);
    help_page_max_scroll(view, width, page_height)
}

pub fn dashboard_max_scroll(view: &View, width: u16, height: u16) -> usize {
    if view.page != crate::app::Page::Dashboard || width < 60 || height < 16 {
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
    input: &'a crate::app::LineInput,
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

fn render_overlay(frame: &mut Frame, view: &View, area: Rect, palette: Palette) {
    let Some(overlay) = &view.overlay else {
        return;
    };
    match overlay {
        Overlay::Edit(form) => render_edit_form(frame, view.text(), form, area, palette),
        Overlay::Setup { selected } => render_setup_popup(frame, view, *selected, area, palette),
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

fn render_footer(frame: &mut Frame, view: &View, area: ratatui::layout::Rect, palette: Palette) {
    let text = view.text();
    // Keep two blank cells free at both edges so hints never touch the border.
    let area = Rect {
        x: area.x.saturating_add(2),
        width: area.width.saturating_sub(4),
        ..area
    };
    let mut spans = if view.restart_in_progress {
        vec![Span::styled(
            text.msg_restarting,
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
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;
    use crate::app::{
        settings_items, App, EditForm, KeywordField, KeywordForm, LineInput, Overlay, SettingsForm,
        SettingsItem, SubscriptionForm,
    };
    use crate::config::{Config, Paths};
    use crate::i18n::Lang;
    use crate::logging::{LogLevel, LogTimestamp, Logger};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tempfile::TempDir;

    async fn test_app(lang: Lang) -> (App, TempDir) {
        let dir = TempDir::new().unwrap();
        let paths = Paths::discover(
            Some(dir.path().join("config.toml")),
            Some(dir.path().join("data")),
        )
        .unwrap();
        paths.ensure().unwrap();
        let mut config = Config::default();
        config.language = lang;
        let logger = Logger::new(&paths.logs_dir, LogLevel::Info, LogLevel::Info).unwrap();
        let mut app = App::new(config, paths, logger).await;
        app.overlay = None;
        (app, dir)
    }

    fn rendered(app: &App, height: u16) -> String {
        rendered_with_width(app, 100, height)
    }

    fn rendered_with_width(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn rendered_rows(view: &View, width: u16, height: u16) -> Vec<Vec<String>> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, view)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol().to_string()).collect())
            .collect()
    }

    #[test]
    fn setup_labels_pad_to_the_configured_width() {
        for label in ["本机 IP", "包含关键词", "Local IP", "Include keywords"] {
            let line = setup_lines(label, "value".to_string(), 16, 40)
                .into_iter()
                .next()
                .expect("line");
            assert_eq!(
                UnicodeWidthStr::width(line.spans[0].content.as_ref()),
                16,
                "{label}"
            );
        }
    }

    #[test]
    fn dashboard_labels_keep_two_spaces_before_values() {
        let line = key_value_wrapped("Listen", "value".to_string(), 80)
            .into_iter()
            .next()
            .expect("line");
        assert_eq!(
            UnicodeWidthStr::width(line.spans[0].content.as_ref()),
            DASHBOARD_LABEL_WIDTH
        );
        for lang in [Lang::Zh, Lang::En] {
            let text = lang.text();
            for label in [
                text.listen,
                text.selection_mode,
                text.current_node,
                text.latency,
                text.local_ip,
                text.proxy_ip,
                text.version,
                text.binary_version,
                text.subscription_summary,
                text.last_error,
            ] {
                let padding = DASHBOARD_LABEL_WIDTH.saturating_sub(UnicodeWidthStr::width(label));
                assert!(
                    padding >= 2,
                    "{lang:?}: {label} leaves {padding} blank columns"
                );
            }
        }
    }

    #[tokio::test]
    async fn renders_chinese_by_default() {
        let (app, _dir) = test_app(Lang::Zh).await;
        let content = rendered(&app, 30);
        assert!(content.contains("DFRelay"));
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("监听"));
    }

    #[tokio::test]
    async fn renders_english_when_selected() {
        let (app, _dir) = test_app(Lang::En).await;
        let content = rendered(&app, 30);
        assert!(content.contains("Listen"));
    }

    #[tokio::test]
    async fn setup_wrapped_values_stay_aligned_with_the_value_column() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.binary_installed = true;
        app.binary_version = Some("1.14.4".to_string());
        app.paths.bin_dir = std::path::PathBuf::from(format!("/tmp/{}", "a".repeat(100)));
        let view = app.view();
        let rows = rendered_rows(&view, 60, 30);
        let current = rows
            .iter()
            .find(|row| row.concat().contains("Current version"))
            .expect("current version row");
        let value_col = current
            .windows(7)
            .position(|cells| cells.concat() == "v1.14.4")
            .expect("version value column");
        let path_row = rows
            .iter()
            .position(|row| row.concat().contains("sing-box path"))
            .expect("binary path row");
        let continuation = &rows[path_row + 1];
        let continuation_col = continuation
            .iter()
            .position(|cell| !matches!(cell.as_str(), " " | "│"))
            .expect("wrapped path continuation");
        assert_eq!(continuation_col, value_col, "{:?}", continuation.concat());
    }

    #[tokio::test]
    async fn setup_page_shows_versions_status_and_neutral_hint() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.binary_installed = true;
        app.binary_version = Some("1.14.4".to_string());
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("当前版本v1.14.4"), "{compact}");
        assert!(compact.contains("状态已安装"), "{compact}");
        assert!(compact.contains("sing-box日志INFO"), "{compact}");
        assert!(!compact.contains("[RO]"), "{compact}");
        assert!(compact.contains("sing-box路径"), "{compact}");
        assert!(compact.contains("按d下载最新版"), "{compact}");
        assert!(compact.contains("j/k选择"), "{compact}");
        assert!(compact.contains("Enter安装"), "{compact}");
        assert!(compact.contains("b切换"), "{compact}");
        assert!(!compact.contains("自动下载失败"), "{compact}");

        app.latest_binary_version = Some("1.15.0".to_string());
        app.previous_binary_available = true;
        app.previous_binary_version = Some("1.14.0".to_string());
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("最新版本v1.15.0"), "{compact}");
        assert!(compact.contains("上一版本v1.14.0"), "{compact}");
        assert!(compact.contains("状态可升级"), "{compact}");

        app.rollback_confirm = true;
        let content = rendered(&app, 30);
        let last_line: String = content.chars().skip(100 * 29).collect();
        let compact: String = last_line.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("再按b切换版本"), "{last_line}");

        app.rollback_confirm = false;
        app.setup.rollback_in_flight = true;
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("正在切换sing-box"), "{compact}");
    }

    #[tokio::test]
    async fn english_settings_and_setup_hints_fit_at_eighty_columns() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Settings;
        let content = rendered_with_width(&app, 80, 24);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("j/kselect"), "{compact}");
        assert!(compact.contains("Enteraction"), "{compact}");

        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.binary_installed = true;
        app.binary_version = Some("1.14.4".to_string());
        let content = rendered_with_width(&app, 80, 24);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("sing-boxlogINFO"), "{compact}");
        assert!(!compact.contains("[RO]"), "{compact}");
        assert!(compact.contains("sing-boxpath"), "{compact}");
        assert!(compact.contains("j/kselect"), "{compact}");
        assert!(compact.contains("Enterinstall"), "{compact}");
        assert!(compact.contains("bprev"), "{compact}");
        assert!(!compact.contains("dupdate"), "{compact}");
        assert!(!compact.contains("Escback"), "{compact}");
    }

    #[tokio::test]
    async fn setup_fits_key_hints_at_eighty_columns() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.binary_installed = true;
        app.binary_version = Some("1.14.4".to_string());
        app.latest_binary_version = Some("1.15.0".to_string());
        app.previous_binary_available = true;
        app.previous_binary_version = Some("1.14.0".to_string());
        let content = rendered_with_width(&app, 80, 24);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("sing-box日志"), "{compact}");
        assert!(compact.contains("sing-box路径"), "{compact}");
        assert!(compact.contains("j/k选择"), "{compact}");
        assert!(compact.contains("Enter安装"), "{compact}");
        assert!(compact.contains("b切换"), "{compact}");
        assert_eq!(compact.matches("Enter安装").count(), 1, "{compact}");
    }

    #[tokio::test]
    async fn setup_shows_binary_verification_state() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.binary_installed = true;
        app.binary_version = Some("1.14.4".to_string());
        app.binary_verified = Some(false);
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(content.contains("校验未校验"), "{content}");

        app.binary_verified = Some(true);
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(content.contains("校验已校验"), "{content}");
    }

    #[tokio::test]
    async fn setup_hides_current_version_when_not_installed() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.binary_installed = false;
        app.latest_binary_version = Some("1.15.0".to_string());
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!compact.contains("当前版本"), "{compact}");
        assert!(!compact.contains("上一版本"), "{compact}");
        assert!(compact.contains("最新版本v1.15.0"), "{compact}");
        assert_eq!(compact.matches("未安装").count(), 1, "{compact}");
        assert!(!compact.contains("[RO]"), "{compact}");
    }

    #[tokio::test]
    async fn setup_keeps_two_spaces_between_english_labels_and_values() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.binary_installed = true;
        app.binary_version = Some("1.14.4".to_string());
        app.previous_binary_available = true;
        app.previous_binary_version = Some("1.14.0".to_string());
        let content = rendered(&app, 30);
        assert!(content.contains("Previous version  v1.14.0"), "{content}");
    }

    #[tokio::test]
    async fn setup_candidates_use_the_standard_selection_style() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.binary_installed = true;
        app.binary_version = Some("1.14.4".to_string());
        app.setup.candidates = vec![
            crate::singbox::binary::InboxCandidate {
                path: std::path::PathBuf::from("/inbox/cand-a.tar.gz"),
                label: "cand-a.tar.gz".to_string(),
                has_checksum: true,
            },
            crate::singbox::binary::InboxCandidate {
                path: std::path::PathBuf::from("/inbox/cand-b.tar.gz"),
                label: "cand-b.tar.gz".to_string(),
                has_checksum: false,
            },
        ];
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(100).collect();

        let selected_row = rows
            .iter()
            .find(|row| row.iter().any(|cell| cell.symbol() == "▸"))
            .expect("selected candidate row");
        let marker = selected_row
            .iter()
            .position(|cell| cell.symbol() == "▸")
            .expect("selection marker");
        assert_eq!(marker, 8);
        assert_eq!(selected_row[marker].fg, Color::Black);
        assert_eq!(selected_row[marker].bg, Color::Cyan);
        assert_eq!(
            selected_row
                .iter()
                .filter(|cell| cell.bg == Color::Cyan)
                .count(),
            84
        );

        let unselected_row = rows
            .iter()
            .find(|row| row.iter().any(|cell| cell.symbol() == "!"))
            .expect("unchecked candidate row");
        let bang = unselected_row
            .iter()
            .position(|cell| cell.symbol() == "!")
            .expect("warning marker");
        assert_eq!(unselected_row[bang].fg, Color::Yellow);
    }

    #[tokio::test]
    async fn setup_unverified_install_confirmation_is_in_the_footer() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.setup.pending_install = Some(std::path::PathBuf::from("/inbox/raw"));
        let content = rendered(&app, 30);
        let last_line: String = content.chars().skip(100 * 29).collect();
        let compact: String = last_line.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("再按Enter安装未校验文件"), "{last_line}");
        assert!(compact.contains("Esc取消"), "{last_line}");
    }

    #[tokio::test]
    async fn setup_shows_installing_status() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.setup.installing = true;
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("正在安装sing-box"), "{compact}");
        assert!(!compact.contains("按d下载最新版"), "{compact}");
    }

    #[tokio::test]
    async fn setup_title_hints_fit_at_sixty_columns_in_both_languages() {
        for (lang, hints) in [
            (Lang::Zh, "j/k选择Enter安装b切换"),
            (Lang::En, "j/kselectEnterinstallbprev"),
        ] {
            let (mut app, _dir) = test_app(lang).await;
            app.overlay = Some(Overlay::Setup { selected: 0 });
            app.binary_installed = true;
            app.binary_version = Some("1.14.4".to_string());
            let content = rendered_with_width(&app, 60, 24);
            let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(compact.contains(hints), "{lang:?}: {compact}");
        }
    }

    #[tokio::test]
    async fn setup_download_status_is_not_duplicated() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.setup.downloading = true;
        app.setup.message = None;
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(compact.matches("正在下载sing-box").count(), 1, "{compact}");
    }

    #[tokio::test]
    async fn settings_shows_singbox_info_status() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        app.selected = settings_items(app.config.allow_lan)
            .iter()
            .position(|item| *item == SettingsItem::BinaryInfo)
            .unwrap();
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("sing-box信息"), "{compact}");
        assert!(compact.contains("未安装"), "{compact}");

        app.binary_installed = true;
        app.binary_version = Some("1.14.4".to_string());
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("已安装"), "{compact}");

        app.latest_binary_version = Some("1.15.0".to_string());
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("可升级"), "{compact}");
    }

    #[tokio::test]
    async fn settings_shows_app_update_policy_and_pending_version() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        app.selected = settings_items(app.config.allow_lan)
            .iter()
            .position(|item| *item == SettingsItem::AppUpdate)
            .unwrap();
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("应用更新"), "{compact}");
        assert!(compact.contains("通知"), "{compact}");

        app.config.app_update = UpdatePolicy::Auto;
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("自动"), "{compact}");
    }

    #[tokio::test]
    async fn header_uses_fixed_title_and_status_value() {
        let (app, _dir) = test_app(Lang::Zh).await;
        let content = rendered(&app, 30);
        assert!(content.contains("DFRelay"));
        assert!(!content.contains("终端代理管理器"));
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("已停止"), "{compact}");
        assert!(!compact.contains("状态:"), "{compact}");
        assert!(!compact.contains("语言:中文"));
        assert!(!compact.contains("sing-box:—"));
    }

    #[tokio::test]
    async fn local_ip_is_only_rendered_on_dashboard_and_one_per_line() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        app.local_ips = vec![
            "192.0.2.10 (eth0)".to_string(),
            "2001:db8::10 (eth0)".to_string(),
        ];
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("本机IP192.0.2.10(eth0)"), "{compact}");
        assert!(compact.contains("2001:db8::10(eth0)"), "{compact}");
        assert!(
            !compact.contains("192.0.2.10(eth0)/2001:db8::10"),
            "{compact}"
        );

        let header: String = content.chars().take(100 * 6).collect();
        let header: String = header.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!header.contains("本机IP"));
    }

    fn find_cell(row: &[&str], needle: &str) -> Option<usize> {
        let chars: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
        (0..=row.len().saturating_sub(chars.len())).find(|&start| {
            chars
                .iter()
                .enumerate()
                .all(|(offset, ch)| row[start + offset] == ch.as_str())
        })
    }

    /// Finds a label in a rendered row while accounting for the blank cell that
    /// follows a double-width character.
    fn find_cell_text(row: &[String], needle: &str) -> Option<usize> {
        for start in 0..row.len() {
            let mut column = start;
            let mut matched = true;
            for ch in needle.chars() {
                let Some(symbol) = row.get(column) else {
                    matched = false;
                    break;
                };
                if symbol != &ch.to_string() {
                    matched = false;
                    break;
                }
                column += UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
            }
            if matched {
                return Some(start);
            }
        }
        None
    }

    #[tokio::test]
    async fn dashboard_local_ip_values_start_at_the_same_column() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        app.local_ips = vec![
            "192.0.2.10 (eth0)".to_string(),
            "2001:db8::10 (eth0)".to_string(),
        ];
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells: Vec<&str> = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        let rows: Vec<&[&str]> = cells.chunks(100).collect();
        let first_row = rows
            .iter()
            .position(|row| find_cell(row, "192.0.2.10").is_some())
            .unwrap();
        let second_row = rows
            .iter()
            .position(|row| find_cell(row, "2001:db8::10").is_some())
            .unwrap();
        assert_eq!(
            find_cell(rows[first_row], "192.0.2.10"),
            find_cell(rows[second_row], "2001:db8::10")
        );
    }

    #[tokio::test]
    async fn dashboard_proxy_ips_go_on_separate_indented_lines() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        app.proxy_ips = vec!["v4 203.0.113.9".to_string(), "v6 2001:db8::9".to_string()];
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells: Vec<&str> = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        let rows: Vec<&[&str]> = cells.chunks(100).collect();
        let v4_row = rows
            .iter()
            .position(|row| find_cell(row, "203.0.113.9").is_some())
            .expect("v4 proxy ip row");
        let v6_row = rows
            .iter()
            .position(|row| find_cell(row, "2001:db8::9").is_some())
            .expect("v6 proxy ip row");
        assert_ne!(v4_row, v6_row, "proxy IPs must use separate lines");
        assert_eq!(
            find_cell(rows[v4_row], "203.0.113.9"),
            find_cell(rows[v6_row], "2001:db8::9")
        );
    }

    #[tokio::test]
    async fn header_shows_uptime_only_while_running() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        app.status = ProxyStatus::Running;
        app.started_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(3723));
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(content.contains("运行中（01:02:03）"), "{content}");
        assert!(!content.contains("运行时长"), "{content}");

        app.status = ProxyStatus::Stopped;
        app.started_at = None;
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(content.contains("已停止"), "{content}");
        assert!(!content.contains("01:02:03"), "{content}");
    }

    #[tokio::test]
    async fn header_shows_only_status_node_connections_and_network() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        app.proxy_ips = vec!["203.0.113.9".to_string()];
        let content = rendered(&app, 30);
        let header: String = content.chars().take(100 * 4).collect();
        let compact: String = header.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("已停止"), "{compact}");
        assert!(!compact.contains("状态:"), "{compact}");
        assert!(compact.contains("节点"), "{compact}");
        assert!(compact.contains("连接数"), "{compact}");
        assert!(compact.contains("网速"), "{compact}");
        assert!(!compact.contains("监听"), "{compact}");
        assert!(!compact.contains("延迟"), "{compact}");
        assert!(!compact.contains("代理IP"), "{compact}");
        assert!(!compact.contains("运行时长"), "{compact}");
    }

    #[test]
    fn node_columns_keep_fixed_widths_and_an_adaptive_name() {
        assert_eq!(NODE_SOURCE_WIDTH, 14);
        let widths = |total: u16| {
            Layout::horizontal(NODE_COLUMNS)
                .spacing(TABLE_COLUMN_SPACING)
                .split(Rect::new(0, 0, total, 1))
                .iter()
                .map(|column| column.width)
                .collect::<Vec<_>>()
        };
        let fixed = NODE_SOURCE_WIDTH + 11 + 4 + 8;
        let wide = widths(94);
        assert_eq!(wide[1], NODE_SOURCE_WIDTH);
        assert_eq!(wide[2], 11);
        assert_eq!(wide[3], 4);
        assert_eq!(wide[4], 8);
        assert_eq!(wide[0], 94 - 4 * 2 - fixed);

        let narrow = widths(74);
        assert_eq!(narrow[0], 74 - 4 * 2 - fixed);
        assert!(narrow[0] < wide[0]);
    }

    #[test]
    fn subscription_value_widths_follow_the_table_layout() {
        assert_eq!(SUBSCRIPTION_NAME_WIDTH, 14);
        let (url, status) = subscription_value_widths(Rect::new(0, 0, 100, 20));
        assert_eq!(status, 15);
        assert_eq!(url, 49);
        let (narrow_url, narrow_status) = subscription_value_widths(Rect::new(0, 0, 80, 20));
        assert_eq!(narrow_url, 29);
        assert!(narrow_url < url, "{narrow_url}");
        assert_eq!(narrow_status, status);
    }

    #[tokio::test]
    async fn tables_keep_two_spaces_between_columns_and_one_at_the_right_edge() {
        for lang in [Lang::Zh, Lang::En] {
            let (app, _dir) = test_app(lang).await;
            let text = lang.text();
            let mut view = app.view();

            view.page = Page::Nodes;
            view.nodes = vec![crate::view::NodeView {
                tag: "n1".to_string(),
                name: "node".to_string(),
                source: "source".to_string(),
                protocol: "shadowsocks".to_string(),
            }];
            let rows = rendered_rows(&view, 100, 30);
            let header = rows
                .iter()
                .find(|row| {
                    find_cell_text(row, text.nodes_source).is_some()
                        && find_cell_text(row, text.nodes_protocol).is_some()
                })
                .expect("nodes header");
            let source = find_cell_text(header, text.nodes_source).expect("source header");
            let protocol = find_cell_text(header, text.nodes_protocol).expect("protocol header");
            let auto = find_cell_text(header, text.nodes_auto).expect("auto header");
            let latency = find_cell_text(header, text.nodes_latency).expect("latency header");
            assert_eq!(
                protocol,
                source + NODE_SOURCE_WIDTH as usize + 2,
                "{lang:?}"
            );
            assert_eq!(auto, protocol + 11 + 2, "{lang:?}");
            assert_eq!(latency, auto + 4 + 2, "{lang:?}");
            assert_eq!(latency + 8, 98, "{lang:?}");
            assert_eq!(header[98], " ", "{lang:?}");
            let content: String = rows.iter().flatten().map(String::as_str).collect();
            assert!(content.contains("shadowsocks"), "{lang:?}");

            view.page = Page::Subscriptions;
            view.subscriptions = vec![crate::view::SubscriptionView {
                name: "example".to_string(),
                url: "https://example.com/sub".to_string(),
                enabled: true,
                node_count: 8,
                unsupported_count: 0,
                last_updated: Some("2026-10-31 12:34:56".to_string()),
                error: None,
                stale: false,
            }];
            let rows = rendered_rows(&view, 100, 30);
            let header = rows
                .iter()
                .find(|row| find_cell_text(row, text.subscriptions_updated).is_some())
                .expect("subscriptions header");
            let name = find_cell_text(header, text.subscriptions_name).expect("name header");
            let url = find_cell_text(header, text.subscriptions_url).expect("url header");
            let nodes = find_cell_text(header, text.subscriptions_nodes).expect("nodes header");
            let updated =
                find_cell_text(header, text.subscriptions_updated).expect("updated header");
            assert_eq!(url, name + SUBSCRIPTION_NAME_WIDTH as usize + 2, "{lang:?}");
            let (url_width, _) = subscription_value_widths(Rect::new(0, 0, 100, 30));
            assert_eq!(nodes, url + url_width + 2, "{lang:?}");
            assert_eq!(updated, nodes + 10 + 2, "{lang:?}");
            assert_eq!(updated + 15, 98, "{lang:?}");
            assert_eq!(header[98], " ", "{lang:?}");
            let content: String = rows.iter().flatten().map(String::as_str).collect();
            assert!(content.contains("10-31 12:34:56"), "{lang:?}");

            view.page = Page::Connections;
            view.metrics.connections = vec![connection()];
            let rows = rendered_rows(&view, 100, 30);
            let header = rows
                .iter()
                .find(|row| {
                    find_cell_text(row, text.connections_dest).is_some()
                        && find_cell_text(row, text.connections_traffic).is_some()
                })
                .expect("connections header");
            let dest = find_cell_text(header, text.connections_dest).expect("destination header");
            let network = find_cell_text(header, text.connections_network).expect("network header");
            let traffic = find_cell_text(header, text.connections_traffic).expect("traffic header");
            let duration =
                find_cell_text(header, text.connections_duration).expect("duration header");
            let columns = Layout::horizontal(CONNECTION_COLUMNS)
                .spacing(TABLE_COLUMN_SPACING)
                .split(Rect::new(0, 0, 94, 1));
            let dest_width = columns[0].width as usize;
            assert_eq!(network, dest + dest_width + 2, "{lang:?}");
            assert_eq!(traffic, network + 7 + 2, "{lang:?}");
            assert_eq!(duration, traffic + 21 + 2, "{lang:?}");
            assert_eq!(duration + 9, 98, "{lang:?}");
            assert_eq!(header[98], " ", "{lang:?}");
            let content: String = rows.iter().flatten().map(String::as_str).collect();
            assert!(content.contains("↓ 2.0 K ↑ 1.0 K"), "{lang:?}");
            assert!(content.contains("1:02:05"), "{lang:?}");
        }
    }

    #[test]
    fn subscription_times_drop_the_year_for_display() {
        assert_eq!(
            display_subscription_time("2026-10-31 12:34:56"),
            "10-31 12:34:56"
        );
        assert_eq!(display_subscription_time("yesterday"), "yesterday");
    }

    #[tokio::test]
    async fn subscriptions_scroll_long_name_url_and_status_text() {
        let (app, _dir) = test_app(Lang::En).await;
        let mut view = app.view();
        view.page = Page::Subscriptions;
        let name = format!("subscription-{}", "n".repeat(20));
        let url = format!("https://example.com/{}", "0123456789".repeat(8));
        let status = format!("10-31 12:34:56 {}", "x".repeat(30));
        view.subscriptions = vec![crate::view::SubscriptionView {
            name: name.clone(),
            url: url.clone(),
            enabled: true,
            node_count: 8,
            unsupported_count: 0,
            last_updated: Some(status.clone()),
            error: None,
            stale: false,
        }];
        let (url_width, status_width) = subscription_value_widths(Rect::new(0, 0, 100, 30));
        let name_width = SUBSCRIPTION_NAME_WIDTH as usize;
        let first_name = marquee_text(&name, name_width, 0);
        let first_url = marquee_text(&url, url_width, 0);
        let first_status = marquee_text(&status, status_width, 0);
        let tick = MARQUEE_PAUSE_TICKS + MARQUEE_STEP_TICKS * 4;
        let moved_name = marquee_text(&name, name_width, tick);
        let moved_url = marquee_text(&url, url_width, tick);
        let moved_status = marquee_text(&status, status_width, tick);
        assert_ne!(first_name, moved_name);
        assert_ne!(first_url, moved_url);
        assert_ne!(first_status, moved_status);

        let rendered = |tick| {
            let backend = TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).unwrap();
            let view = view.clone();
            terminal
                .draw(|frame| render_at(frame, &view, tick, Palette::Dark))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert!(rendered(0).contains(&first_name), "{first_name}");
        assert!(rendered(0).contains(&first_url), "{first_url}");
        assert!(rendered(0).contains(&first_status), "{first_status}");
        assert!(rendered(tick).contains(&moved_name), "{moved_name}");
        assert!(rendered(tick).contains(&moved_url), "{moved_url}");
        assert!(rendered(tick).contains(&moved_status), "{moved_status}");
    }

    #[tokio::test]
    async fn nodes_scroll_long_source_names_inside_the_column() {
        let (app, _dir) = test_app(Lang::En).await;
        let mut view = app.view();
        view.page = Page::Nodes;
        let source = format!("source-{}", "s".repeat(24));
        view.nodes = vec![crate::view::NodeView {
            tag: "n1".to_string(),
            name: "node".to_string(),
            source: source.clone(),
            protocol: "ss".to_string(),
        }];
        let width = NODE_SOURCE_WIDTH as usize;
        let first = marquee_text(&source, width, 0);
        let tick = MARQUEE_PAUSE_TICKS + MARQUEE_STEP_TICKS * 4;
        let moved = marquee_text(&source, width, tick);
        assert_ne!(first, moved);

        let rendered = |tick| {
            let backend = TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).unwrap();
            let view = view.clone();
            terminal
                .draw(|frame| render_at(frame, &view, tick, Palette::Dark))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert!(rendered(0).contains(&first), "{first}");
        assert!(rendered(tick).contains(&moved), "{moved}");
    }

    #[test]
    fn marquee_scrolls_truncated_text_back_and_forth() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        assert_eq!(marquee_text(text, 10, 0), "abcdefghij");
        assert_eq!(marquee_text(text, text.len(), 0), text);
        let forward = MARQUEE_PAUSE_TICKS + MARQUEE_STEP_TICKS * 2;
        assert_eq!(marquee_text(text, 10, forward), "cdefghijkl");
        let at_end = MARQUEE_PAUSE_TICKS + (text.len() - 10) as u64 * MARQUEE_STEP_TICKS;
        assert_eq!(marquee_text(text, 10, at_end), "qrstuvwxyz");
        let back = 2 * MARQUEE_PAUSE_TICKS
            + (text.len() - 10) as u64 * MARQUEE_STEP_TICKS
            + MARQUEE_STEP_TICKS;
        assert_eq!(marquee_text(text, 10, back), "pqrstuvwxy");
    }

    #[test]
    fn marquee_respects_display_width_for_wide_characters() {
        let text = "订阅链接https://example.com/very-long-path";
        let rendered = marquee_text(text, 12, MARQUEE_PAUSE_TICKS);
        assert!(UnicodeWidthStr::width(rendered.as_str()) <= 12);
        assert!(!rendered.is_empty());
    }

    #[test]
    fn settings_values_wrap_and_indent_under_the_value_column() {
        let label_width = 18;
        let value_width = 30;
        let value = format!(
            "/home/example/.local/share/dfrelay/bin/{}/sing-box",
            "a".repeat(50)
        );
        let lines = settings_lines(
            false,
            "sing-box 路径",
            value,
            None,
            label_width,
            value_width,
            Palette::Dark,
        );
        assert!(lines.len() > 1, "long settings value should wrap");
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            rendered[1].starts_with(&" ".repeat(2 + label_width)),
            "{:?}",
            rendered[1]
        );
        for line in &rendered {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 2 + label_width + value_width,
                "{line:?}"
            );
        }
        let joined: String = rendered.join("");
        let compact: String = joined.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(compact.ends_with("/sing-box"), "{compact}");
    }

    #[tokio::test]
    async fn footer_uses_default_key_color_and_unbold_keys() {
        let (app, _dir) = test_app(Lang::Zh).await;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(100).collect();
        let footer = rows[29];
        let q = footer
            .iter()
            .position(|cell| cell.symbol() == "q")
            .expect("q shortcut");
        assert_eq!(footer[q].fg, Color::Reset);
        assert!(!footer[q].modifier.contains(Modifier::BOLD));
    }

    #[tokio::test]
    async fn footer_keeps_two_blank_columns_at_both_edges() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Settings;
        app.config.allow_lan = true;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(100).collect();
        let footer = rows[29];
        assert_eq!(footer[0].symbol(), " ");
        assert_eq!(footer[1].symbol(), " ");
        assert_eq!(footer[2].symbol(), "L");
        let last = footer
            .iter()
            .rposition(|cell| cell.symbol() != " ")
            .expect("footer content");
        assert_eq!(last, 97);
    }

    #[tokio::test]
    async fn dark_palette_uses_dark_gray_for_muted_text() {
        let (app, _dir) = test_app(Lang::Zh).await;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal
            .draw(|frame| render_at(frame, &view, 0, Palette::Dark))
            .unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(100).collect();
        let footer = rows[29];
        let label = footer
            .iter()
            .position(|cell| cell.symbol() == "退")
            .expect("quit label");
        assert_eq!(footer[label].fg, Color::DarkGray);
    }

    #[tokio::test]
    async fn light_palette_uses_dark_gray_for_muted_text() {
        let (app, _dir) = test_app(Lang::Zh).await;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal
            .draw(|frame| render_at(frame, &view, 0, Palette::Light))
            .unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(100).collect();
        let footer = rows[29];
        let label = footer
            .iter()
            .position(|cell| cell.symbol() == "退")
            .expect("quit label");
        assert_eq!(footer[label].fg, Color::DarkGray);
    }

    #[tokio::test]
    async fn settings_read_only_items_show_ro_marker() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        app.logger.app(LogLevel::Info, "settings path test");
        let content = rendered(&app, 40);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("sing-box信息"), "{compact}");
        assert!(!compact.contains("sing-box日志"), "{compact}");
        assert!(!compact.contains("sing-box路径"), "{compact}");
        assert!(compact.contains("配置文件"), "{compact}");
        assert!(compact.contains("日志文件"), "{compact}");
        assert!(!compact.contains("[只读]"), "{compact}");
        assert!(compact.contains("[RO]"), "{compact}");
        assert!(
            compact.contains("测试URLhttps://www.gstatic.com/generate_204[RO]"),
            "{compact}"
        );
        assert!(compact.contains("应用日志"), "{compact}");
        assert!(compact.contains("j/k选择"), "{compact}");
    }

    #[tokio::test]
    async fn settings_ro_marker_is_muted_but_the_value_is_not() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Settings;
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(120).collect();
        let row = rows
            .iter()
            .find(|row| {
                let text: String = row.iter().map(|cell| cell.symbol()).collect();
                text.contains("Test URL")
            })
            .expect("Test URL row");
        let marker_start = row
            .iter()
            .position(|cell| cell.symbol() == "[")
            .expect("[RO] marker");
        for cell in &row[marker_start..marker_start + 4] {
            assert_eq!(cell.fg, Color::DarkGray, "marker must be muted");
        }
        assert_eq!(
            row[marker_start - 2].fg,
            Color::Reset,
            "the value must keep the default color"
        );
    }

    #[tokio::test]
    async fn settings_keeps_space_between_english_labels_and_values() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Settings;
        let backend = TestBackend::new(100, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells: Vec<&str> = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        let rows: Vec<&[&str]> = cells.chunks(100).collect();
        let row = rows
            .iter()
            .find(|row| find_cell(row, "Include keywords").is_some())
            .expect("include keyword row");
        let label_start = find_cell(row, "Include keywords").unwrap();
        let value_start = find_cell(row, "all nodes").unwrap();
        assert!(value_start >= label_start + "Include keywords".len() + 2);
    }

    #[tokio::test]
    async fn dashboard_omits_network_and_connections_rows() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        let view = app.view();
        let text: String = dashboard_lines(&view, 80)
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(!text.contains(app.text().network), "{text}");
        assert!(!text.contains(app.text().connections), "{text}");
    }

    #[tokio::test]
    async fn dashboard_shows_selection_mode_above_current_node() {
        for (lang, auto, manual) in [
            (Lang::Zh, "自动选择", "手动指定"),
            (Lang::En, "Auto", "Manual"),
        ] {
            let (mut app, _dir) = test_app(lang).await;
            app.page = Page::Dashboard;
            app.nodes = vec![node("n1", "香港 01")];
            let labels = app.text();

            let lines = |app: &crate::app::App| -> Vec<String> {
                dashboard_lines(&app.view(), 80)
                    .iter()
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                    })
                    .collect()
            };

            app.metrics.selection.selected = Some("auto".to_string());
            let rendered = lines(&app);
            let mode = rendered
                .iter()
                .position(|line| line.contains(labels.selection_mode))
                .expect("selection mode line");
            let current = rendered
                .iter()
                .position(|line| line.contains(labels.current_node))
                .expect("current node line");
            assert_eq!(mode + 1, current, "{lang:?}");
            assert!(
                rendered[mode].contains(auto),
                "{lang:?}: {}",
                rendered[mode]
            );

            app.metrics.selection.selected = Some("n1".to_string());
            let rendered = lines(&app);
            assert!(
                rendered[mode].contains(manual),
                "{lang:?}: {}",
                rendered[mode]
            );

            app.metrics.selection.selected = None;
            app.preferred_node = Some("n1".to_string());
            let rendered = lines(&app);
            assert!(
                rendered[mode].contains(manual),
                "{lang:?}: {}",
                rendered[mode]
            );

            app.preferred_node = None;
            let rendered = lines(&app);
            assert!(
                rendered[mode].contains(auto),
                "{lang:?}: {}",
                rendered[mode]
            );
        }
    }

    #[tokio::test]
    async fn english_subscriptions_use_the_full_label() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Subscriptions;
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(content.contains("Subscriptions"), "{content}");
        assert_eq!(app.text().subscriptions_title, "Subscriptions");
    }

    #[tokio::test]
    async fn logs_timestamp_cycles_time_date_and_hidden() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Logs;
        app.logger.app(LogLevel::Info, "date toggle test");
        let timestamp = app
            .log_lines()
            .first()
            .map(|line| line.timestamp.clone())
            .expect("log timestamp");
        let time = timestamp
            .split_once(' ')
            .map(|(_, time)| time)
            .expect("time of day");
        let content = rendered(&app, 30);
        assert!(content.contains(time), "{content}");
        assert!(!content.contains(&timestamp), "{content}");
        app.log_timestamp = LogTimestamp::DateTime;
        let content = rendered(&app, 30);
        assert!(content.contains(&timestamp), "{content}");
        app.log_timestamp = LogTimestamp::Hidden;
        let content = rendered(&app, 30);
        assert!(!content.contains(time), "{content}");
        assert!(!content.contains(&timestamp), "{content}");
        assert!(
            content.contains("[INFO] [app] date toggle test"),
            "{content}"
        );
    }

    #[tokio::test]
    async fn logs_source_filter_scopes_lines_and_title() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Logs;
        app.logger.app(LogLevel::Info, "app only line");
        app.logger.singbox(LogLevel::Info, "sing-box only line");

        app.log_source = LogSource::App;
        let content = rendered(&app, 30);
        assert!(content.contains("[app] [INFO]"), "{content}");
        assert!(content.contains("app only line"), "{content}");
        assert!(!content.contains("sing-box only line"), "{content}");

        app.log_source = LogSource::Singbox;
        let content = rendered(&app, 30);
        assert!(content.contains("[sing-box] [INFO]"), "{content}");
        assert!(content.contains("sing-box only line"), "{content}");
        assert!(!content.contains("app only line"), "{content}");

        app.logger.clear_view();
        app.log_source = LogSource::App;
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains(app.text().logs_empty_source), "{compact}");
    }

    #[tokio::test]
    async fn tabs_use_compact_spacing() {
        let (app, _dir) = test_app(Lang::En).await;
        let content = rendered(&app, 30);
        assert!(
            content.contains("Dashboard | Nodes | Subscriptions | Connections | Logs | Settings"),
            "{content}"
        );
    }

    #[tokio::test]
    async fn header_marks_a_stale_clash_api() {
        let (app, _dir) = test_app(Lang::Zh).await;
        let mut view = app.view();
        view.status = ProxyStatus::Running;
        view.metrics_stale = true;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(content.contains("API未知"), "{content}");
    }

    #[tokio::test]
    async fn setup_scrolls_the_selected_candidate_into_view() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        app.setup.candidates = (0..30)
            .map(|index| crate::singbox::binary::InboxCandidate {
                path: std::path::PathBuf::from(format!("/inbox/cand-{index}")),
                label: format!("cand-{index:02}"),
                has_checksum: true,
            })
            .collect();
        app.overlay = Some(Overlay::Setup { selected: 29 });
        let content = rendered(&app, 30);
        assert!(content.contains("cand-29"), "{content}");
    }

    #[test]
    fn visible_row_range_follows_the_selection() {
        assert_eq!(visible_row_range(250, 249, 27), (223, 250));
        assert_eq!(visible_row_range(250, 0, 27), (0, 27));
        assert_eq!(visible_row_range(2, 1, 27), (0, 2));
        assert_eq!(visible_row_range(0, 0, 27), (0, 0));
        assert_eq!(visible_row_range(10, 0, 0), (0, 0));
        assert_eq!(visible_row_range(10, 99, 4), (6, 10));
    }

    #[tokio::test]
    async fn nodes_beyond_the_old_render_cap_remain_selectable() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Nodes;
        app.nodes = (0..250)
            .map(|index| crate::subscription::model::Node {
                tag: format!("n-{index:04}"),
                name: format!("node-{index:03}"),
                source: "test".to_string(),
                protocol: "ss".to_string(),
                server: "example.com".to_string(),
                port: 443,
                outbound: serde_json::json!({}),
            })
            .collect();
        app.selected = 249;
        let content = rendered(&app, 30);
        assert!(content.contains("node-249"), "{content}");
    }

    #[tokio::test]
    async fn logs_title_does_not_show_the_log_file_path() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Logs;
        app.logger.app(LogLevel::Info, "logs title test");
        let content = rendered(&app, 24);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("日志[all][INFO]"), "{compact}");
        assert!(!compact.contains("dfrelay-"), "{compact}");
    }

    #[tokio::test]
    async fn renders_settings_form_overlay() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        app.overlay = Some(Overlay::Edit(EditForm::Settings(SettingsForm::new(
            &app.config,
        ))));
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("局域网认证"), "{compact}");
        assert!(compact.contains("用户名"), "{compact}");
        assert!(compact.contains("密码"), "{compact}");
        assert!(compact.contains("Tab/↑↓切字段"), "{compact}");
        assert!(compact.contains("←/→光标"), "{compact}");
        assert!(compact.contains("Enter保存"), "{compact}");
        assert!(compact.contains("Esc取消"), "{compact}");
    }

    #[tokio::test]
    async fn overlays_hide_the_page_background_but_keep_the_header() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Settings;
        app.overlay = Some(Overlay::Setup { selected: 0 });
        let content = rendered(&app, 30);
        assert!(content.contains("DFRelay"));
        assert!(content.contains("sing-box info"));
        assert!(!content.contains("Language"), "{content}");
    }

    #[tokio::test]
    async fn multi_field_edit_forms_mark_the_active_field_and_dim_inactive_labels() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        app.overlay = Some(Overlay::Edit(EditForm::Settings(SettingsForm::new(
            &app.config,
        ))));
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(100).collect();
        let find_row = |label: &str| {
            rows.iter()
                .find(|row| {
                    let text: String = row
                        .iter()
                        .map(|cell| cell.symbol())
                        .filter(|symbol| !symbol.trim().is_empty())
                        .collect();
                    text.contains(label)
                })
                .expect("label row")
        };

        let username_row = find_row("用户名");
        let username_label = username_row
            .iter()
            .position(|cell| cell.symbol() == "用")
            .expect("username label");
        assert_eq!(username_row[username_label - 2].symbol(), "▸");
        assert_eq!(username_row[username_label - 2].fg, Color::Cyan);
        assert_eq!(username_row[username_label].fg, Color::Cyan);
        assert!(username_row[username_label]
            .modifier
            .contains(Modifier::BOLD));

        let password_row = find_row("密码");
        let password_label = password_row
            .iter()
            .position(|cell| cell.symbol() == "密")
            .expect("password label");
        assert_eq!(password_row[password_label - 2].symbol(), " ");
        assert_eq!(password_row[password_label].fg, Color::DarkGray);
    }

    #[tokio::test]
    async fn settings_lan_toggle_reflects_each_value() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!compact.contains("跳过局域网"), "{compact}");
        assert!(compact.contains("局域网访问关"), "{compact}");

        app.toggle_allow_lan();
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("局域网访问开"), "{compact}");
        assert!(compact.contains("局域网认证未设置/未设置"), "{compact}");
    }

    #[tokio::test]
    async fn lan_auth_list_hides_password_and_popup_shows_it() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        app.config.allow_lan = true;
        app.config.lan_username = "alice".to_string();
        app.config.lan_password = "s3cret".to_string();
        let content = rendered(&app, 40);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("局域网认证alice/已设置"), "{compact}");
        assert!(!compact.contains("s3cret"), "{compact}");
        assert!(compact.contains("LAN认证已启用"), "{compact}");

        app.overlay = Some(Overlay::Edit(EditForm::Settings(SettingsForm::new(
            &app.config,
        ))));
        let content = rendered(&app, 40);
        assert!(content.contains("s3cret"), "password should be visible");
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!compact.contains("Ctrl-R"), "{compact}");
        assert!(compact.contains("Tab/↑↓切字段"), "{compact}");
    }

    #[tokio::test]
    async fn settings_lan_warning_is_compact_in_the_footer() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Settings;
        app.config.allow_lan = true;
        let content = rendered_with_width(&app, 60, 24);
        let last_line: String = content.chars().skip(60 * 23).collect();
        let last_line: String = last_line.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(last_line.ends_with("LAN:noauth"), "{last_line}");
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!compact.contains("unauthenticatedproxy"), "{compact}");
    }

    #[tokio::test]
    async fn restart_progress_replaces_footer_hints() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.restart_in_progress = true;
        let content = rendered(&app, 30);
        let last_line: String = content.chars().skip(100 * 29).collect();
        let compact: String = last_line.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("正在重启代理"), "{last_line}");
        assert!(!compact.contains("q退出"), "{last_line}");
    }

    #[tokio::test]
    async fn restart_confirmation_is_shown_in_the_footer() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.restart_confirm = true;
        let content = rendered(&app, 30);
        let last_line: String = content.chars().skip(100 * 29).collect();
        let compact: String = last_line.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("再按R重启"), "{last_line}");
    }

    #[tokio::test]
    async fn delete_confirmation_stays_in_the_footer() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Subscriptions;
        app.subscription_delete = Some("s-1".to_string());
        let content = rendered(&app, 30);
        let last_line: String = content.chars().skip(100 * 29).collect();
        let compact: String = last_line.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("再按d删除"), "{last_line}");
        let header: String = content.chars().take(100 * 4).collect();
        let header: String = header.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!header.contains("再按d删除"), "{header}");
    }

    #[tokio::test]
    async fn overlay_hides_global_footer_hints() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        app.overlay = Some(Overlay::Edit(EditForm::Settings(SettingsForm::new(
            &app.config,
        ))));
        let content = rendered(&app, 30);
        let last_line: String = content.chars().skip(100 * 29).collect();
        assert!(!last_line.contains("Language"), "{last_line}");
        assert!(!last_line.contains("退出"), "{last_line}");
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("Tab/↑↓切字段"), "{compact}");
    }

    #[tokio::test]
    async fn toast_replaces_the_header_slogan() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.notify("已保存");
        let content = rendered(&app, 30);
        let header: String = content
            .chars()
            .take(100 * 4)
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(header.contains("已保存"), "{header}");
        assert!(!header.contains("东风一箭，万里一线"), "{header}");
        let last_line: String = content.chars().skip(100 * 29).collect();
        assert!(!last_line.contains("已保存"), "{last_line}");
    }

    #[tokio::test]
    async fn renders_keyword_form_overlay_with_one_field() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        app.overlay = Some(Overlay::Edit(EditForm::Keyword(KeywordForm::from_config(
            &app.config,
            KeywordField::Include,
        ))));
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("节点关键词"), "{compact}");
        assert!(compact.contains("包含关键词"), "{compact}");
        let form = app.keyword_form().unwrap();
        assert_eq!(form.title(app.text()), app.text().settings_keywords_include);
    }

    #[tokio::test]
    async fn renders_subscription_form_overlay() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Subscriptions;
        app.overlay = Some(Overlay::Edit(EditForm::Subscription(
            SubscriptionForm::new(),
        )));
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("新增订阅"));
        assert!(compact.contains("启用"));
        assert!(compact.contains("[✓]"));
        assert!(!content.contains("[ ]"));

        if let Some(Overlay::Edit(EditForm::Subscription(form))) = &mut app.overlay {
            form.enabled = false;
        }
        let content = rendered(&app, 30);
        assert!(content.contains("[ ]"));
        assert!(!content.contains("[✓]"));
    }

    #[tokio::test]
    async fn subscription_form_values_start_at_the_same_column() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Subscriptions;
        let mut form = SubscriptionForm::new();
        form.name = LineInput::new("alpha");
        form.url = LineInput::new("https://example.com/sub");
        app.overlay = Some(Overlay::Edit(EditForm::Subscription(form)));
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells: Vec<&str> = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        let rows: Vec<&[&str]> = cells.chunks(100).collect();
        let name_row = rows
            .iter()
            .position(|row| find_cell(row, "alpha").is_some())
            .expect("name value");
        let url_row = rows
            .iter()
            .position(|row| find_cell(row, "https://example.com/sub").is_some())
            .expect("url value");
        let enabled_row = rows
            .iter()
            .position(|row| find_cell(row, "[✓]").is_some())
            .expect("enabled value");
        let name_column = find_cell(rows[name_row], "alpha").unwrap();
        assert_eq!(
            name_column,
            find_cell(rows[url_row], "https://example.com/sub").unwrap()
        );
        assert_eq!(name_column, find_cell(rows[enabled_row], "[✓]").unwrap());
    }

    #[test]
    fn long_url_wraps_under_its_value_column() {
        let long_url = format!("https://example.com/{}", "a".repeat(120));
        let input = LineInput::new(long_url);
        let lines = input_lines(
            InputField {
                marker: "▸ ",
                label: "URL",
                label_width: 3,
                input: &input,
                focused: true,
                label_style: Style::default(),
            },
            40,
        );
        assert!(lines.len() > 1, "long URL should wrap");
        let indent = lines[1].spans[0].content.as_ref();
        assert!(
            !indent.is_empty() && indent.chars().all(|ch| ch == ' '),
            "continuation line should be indented: {indent:?}"
        );
        assert_eq!(
            UnicodeWidthStr::width(indent),
            UnicodeWidthStr::width("▸ URL: ")
        );
        let all: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(all.contains("https://example.com/"));
        assert!(all.matches('▏').count() == 1);
    }

    #[tokio::test]
    async fn header_shows_the_slogan_on_the_right() {
        let (app, _dir) = test_app(Lang::Zh).await;
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(content.contains("东风一箭，万里一线"), "{content}");

        let (app, _dir) = test_app(Lang::En).await;
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            content.contains("Twelvethousandinflight,theworldinsight"),
            "{content}"
        );
    }

    #[tokio::test]
    async fn dashboard_scroll_reveals_overflowing_lines() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        app.local_ips = (1..=20)
            .map(|index| format!("192.0.2.{index} (en0)"))
            .collect();
        let view = app.view();
        assert!(dashboard_max_scroll(&view, 80, 24) > 0);
        assert_eq!(dashboard_max_scroll(&view, 80, 80), 0);

        app.dashboard_scroll = 10;
        let content: String = rendered_with_width(&app, 80, 24)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(!content.contains("监听"), "{content}");
        assert!(content.contains("192.0.2.15(en0)"), "{content}");
    }

    #[tokio::test]
    async fn dashboard_wraps_long_values_under_the_value_column() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Dashboard;
        app.last_error = Some(
            "a deliberately long error message that must wrap instead of being cropped by the dashboard"
                .to_string(),
        );
        let view = app.view();
        let width = 40;
        let lines: Vec<String> = dashboard_lines(&view, width)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        let error_index = lines
            .iter()
            .position(|line| line.contains("Last error"))
            .expect("last error line");
        assert!(
            lines.len() > error_index + 1,
            "long error should occupy multiple lines"
        );
        let indent = " ".repeat(DASHBOARD_LABEL_WIDTH);
        for line in &lines[error_index + 1..] {
            assert!(
                line.starts_with(&indent),
                "continuation not indented: {line:?}"
            );
        }
        for line in &lines {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= width,
                "line exceeds dashboard width: {line:?}"
            );
        }
    }

    #[tokio::test]
    async fn dashboard_marks_the_last_error_red_and_the_header_stays_clean() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Dashboard;
        app.last_error = Some(
            "a deliberately long error message that must wrap instead of being cropped by the dashboard"
                .to_string(),
        );
        let view = app.view();
        let lines = dashboard_lines(&view, 40);
        let error_index = lines
            .iter()
            .position(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("Last error"))
            })
            .expect("last error line");
        assert!(error_index + 1 < lines.len(), "long error should wrap");
        for line in &lines[error_index..] {
            assert_eq!(
                line.style.fg,
                Some(Color::Red),
                "last error line is not red: {line:?}"
            );
        }

        let compact: String = rendered(&app, 30)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert_eq!(
            compact.matches("Lasterror").count(),
            1,
            "header repeats the persistent error: {compact}"
        );

        for (width, height) in [(80, 24), (120, 40)] {
            let rows = rendered_rows(&view, width, height);
            let tabs: String = rows[4].join("");
            assert!(
                tabs.contains("Dashboard"),
                "header grew past two lines at {width}x{height}: {tabs}"
            );
            let all: String = rows.concat().join("");
            assert_eq!(
                all.matches("Last error").count(),
                1,
                "persistent error rendered outside the dashboard at {width}x{height}"
            );
        }

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut found_error_row = false;
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer.content[(y * buffer.area.width + x) as usize].symbol())
                .collect();
            if !row.contains("Last error") {
                continue;
            }
            found_error_row = true;
            for x in 0..buffer.area.width {
                let cell = &buffer.content[(y * buffer.area.width + x) as usize];
                if cell.symbol().trim().is_empty() || cell.symbol() == "│" {
                    continue;
                }
                assert_eq!(cell.fg, Color::Red, "last error row is not red: {row}");
            }
        }
        assert!(found_error_row, "dashboard did not render the last error");

        app.last_error = None;
        let view = app.view();
        let lines = dashboard_lines(&view, 80);
        let error_index = lines
            .iter()
            .position(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("Last error"))
            })
            .expect("last error line");
        assert_ne!(lines[error_index].style.fg, Some(Color::Red));
    }

    #[tokio::test]
    async fn dashboard_shows_the_active_node_delay() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        app.metrics.selection.selected = Some("n2".to_string());
        app.node_delays.insert("n2".to_string(), 88);
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(content.contains("88ms"), "{content}");

        app.metrics.selection.selected = Some("auto".to_string());
        app.metrics.selection.auto_now = Some("n1".to_string());
        app.node_delays.insert("n1".to_string(), 42);
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(content.contains("42ms"), "{content}");
    }

    #[tokio::test]
    async fn dashboard_shows_app_version_and_prefixed_singbox_version() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        app.binary_version = Some("1.14.4".to_string());
        let content: String = rendered(&app, 40)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            content.contains(crate::version::DISPLAY),
            "version display missing from dashboard: {content}"
        );
        assert!(content.contains("v1.14.4"), "{content}");
    }

    #[tokio::test]
    async fn footer_shows_the_language_it_switches_to() {
        let (app, _dir) = test_app(Lang::Zh).await;
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(content.contains("LLanguage"));
        assert!(!content.contains("L语言"));

        let (app, _dir) = test_app(Lang::En).await;
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(content.contains("L语言"));
    }

    fn connection() -> crate::singbox::clash::ConnectionInfo {
        crate::singbox::clash::ConnectionInfo {
            id: "id1".to_string(),
            host: "example.com".to_string(),
            destination_ip: "203.0.113.5".to_string(),
            destination_port: "443".to_string(),
            network: "tcp".to_string(),
            upload: 1024,
            download: 2048,
            duration_secs: Some(3725),
        }
    }

    fn node(tag: &str, name: &str) -> crate::subscription::model::Node {
        crate::subscription::model::Node {
            tag: tag.to_string(),
            name: name.to_string(),
            source: "test".to_string(),
            protocol: "ss".to_string(),
            server: "example.com".to_string(),
            port: 443,
            outbound: serde_json::json!({}),
        }
    }

    #[test]
    fn connection_destination_adapts_while_traffic_keeps_its_fixed_width() {
        let columns = Layout::horizontal(CONNECTION_COLUMNS)
            .spacing(TABLE_COLUMN_SPACING)
            .split(Rect::new(0, 0, 94, 1));
        assert_eq!(columns[0].width, 94 - 3 * 2 - 7 - 21 - 9);
        assert_eq!(columns[1].width, 7);
        assert_eq!(columns[2].width, 21);
        assert_eq!(columns[3].width, 9);
    }

    #[test]
    fn rates_use_compact_binary_suffixes() {
        // Formatting itself is covered by ui::format tests; this asserts the
        // helpers remain reachable from the page renderer.
        assert_eq!(format_rate(1024.0), "1.0 K/s");
    }

    #[test]
    fn byte_counts_use_compact_binary_suffixes() {
        assert_eq!(format_bytes(1024), "1.0 K");
    }

    #[test]
    fn connection_durations_are_compact_clock_values() {
        assert_eq!(format_duration(None), "—");
        assert_eq!(format_duration(Some(0)), "00:00");
        assert_eq!(format_duration(Some(59)), "00:59");
        assert_eq!(format_duration(Some(3725)), "1:02:05");
        assert_eq!(format_duration(Some(90000)), "25:00:00");
    }

    #[test]
    fn connection_target_keeps_only_one_address() {
        let mut item = connection();
        assert_eq!(connection_target(&item), "example.com:443");

        item.host.clear();
        assert_eq!(connection_target(&item), "203.0.113.5:443");

        item.destination_ip.clear();
        assert_eq!(connection_target(&item), "—");
    }

    #[tokio::test]
    async fn connections_have_one_target_column_and_no_summary() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Connections;
        app.metrics.connections.push(connection());
        let raw = rendered(&app, 30);
        assert!(raw.contains("↓ 2.0 K ↑ 1.0 K"), "{raw}");
        let content: String = raw.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(content.contains("目标"), "{content}");
        assert!(content.contains("example.com:443"), "{content}");
        assert!(!content.contains("主机"), "{content}");
        assert!(!content.contains("规则"), "{content}");
        assert!(!content.contains("id1"), "{content}");
        assert!(!content.contains("/usr/bin/curl"), "{content}");
    }

    #[tokio::test]
    async fn switching_to_logs_does_not_keep_the_connection_summary() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Connections;
        app.metrics.connections.push(connection());
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

        app.page = Page::Logs;
        app.logger.app(LogLevel::Info, "log after switching");
        terminal.clear().unwrap();
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        let compact: String = content.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(compact.contains("日志"), "{content}");
        assert!(!content.contains("id1"), "{content}");
        assert!(!content.contains("/usr/bin/curl"), "{content}");
        assert!(!content.contains("127.0.0.1:50000"), "{content}");
    }

    #[tokio::test]
    async fn nodes_show_latency_for_every_node() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Nodes;
        app.nodes = vec![node("n1", "香港 01"), node("n2", "日本 02")];
        app.node_delays.insert("n1".to_string(), 68);
        app.node_delays.insert("n2".to_string(), 120);
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(content.contains("68ms"), "{content}");
        assert!(content.contains("120ms"), "{content}");

        app.node_delays.clear();
        app.delay_testing.insert("n1".to_string());
        let content: String = rendered(&app, 30)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(content.contains('…'), "{content}");
    }

    #[tokio::test]
    async fn nodes_mark_a_single_active_node() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Nodes;
        app.nodes = vec![node("n1", "hk-01"), node("n2", "jp-02")];

        // Manual selection wins over a different urltest result; previously
        // both rows carried a dot in this state.
        app.metrics.selection.selected = Some("n1".to_string());
        app.metrics.selection.auto_now = Some("n2".to_string());
        app.preferred_node = Some("n1".to_string());
        let rows = rendered_rows(&app.view(), 100, 30);
        let marks: Vec<String> = rows
            .iter()
            .map(|row| row.concat())
            .filter(|row| row.contains('◉'))
            .collect();
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert!(marks[0].contains("hk-01"), "{marks:?}");

        // Auto mode marks the urltest result only.
        app.metrics.selection.selected = Some("auto".to_string());
        app.preferred_node = None;
        let rows = rendered_rows(&app.view(), 100, 30);
        let marks: Vec<String> = rows
            .iter()
            .map(|row| row.concat())
            .filter(|row| row.contains('◉'))
            .collect();
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert!(marks[0].contains("jp-02"), "{marks:?}");

        // Without metrics the pending preferred node is marked.
        app.metrics = crate::singbox::clash::MetricsSnapshot::default();
        app.preferred_node = Some("n2".to_string());
        let rows = rendered_rows(&app.view(), 100, 30);
        let marks: Vec<String> = rows
            .iter()
            .map(|row| row.concat())
            .filter(|row| row.contains('◉'))
            .collect();
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert!(marks[0].contains("jp-02"), "{marks:?}");
    }

    #[tokio::test]
    async fn nodes_dot_keeps_one_cell_and_one_blank_before_the_name() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Nodes;
        app.nodes = vec![node("n1", "hk-01")];
        app.metrics.selection.selected = Some("n1".to_string());
        let rows = rendered_rows(&app.view(), 100, 30);
        // Match the table row (it also carries the source "test"), not the
        // status bar, which shows the current node name as well.
        let row = rows
            .iter()
            .find(|row| {
                let line = row.concat();
                line.contains("hk-01") && line.contains("test")
            })
            .expect("node row");
        let dot = row.iter().position(|cell| cell == "◉").expect("dot");
        assert_eq!(row[dot + 1], " ", "one blank cell after the dot");
        assert_eq!(row[dot + 2], "h", "name starts right after the gap");
        let header = rows
            .iter()
            .find(|row| row.concat().contains("Source"))
            .expect("header row");
        let label = header
            .iter()
            .position(|cell| cell == "N")
            .expect("name label");
        assert_eq!(label, dot + 2, "header label aligns with names");
    }

    #[tokio::test]
    async fn nodes_active_row_uses_the_tab_accent_without_bold() {
        let (mut app, _dir) = test_app(Lang::En).await;
        app.page = Page::Nodes;
        app.nodes = vec![
            node("n1", "hk-01"),
            node("n2", "jp-02"),
            node("n3", "sg-03"),
        ];
        app.metrics.selection.selected = Some("n1".to_string());
        // Keep the cursor off the active row so its own style is visible.
        app.selected = 2;

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = app.view();
        terminal.draw(|frame| render(frame, &view)).unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(100).collect();

        let active = rows
            .iter()
            .find(|row| row.iter().any(|cell| cell.symbol() == "◉"))
            .expect("active node row");
        let dot = active
            .iter()
            .position(|cell| cell.symbol() == "◉")
            .expect("active dot");
        for cell in active[dot..].iter().take_while(|cell| cell.symbol() != "│") {
            if cell.symbol() != " " {
                assert_eq!(cell.fg, Color::Cyan, "{cell:?}");
                assert!(!cell.modifier.contains(Modifier::BOLD), "{cell:?}");
            }
        }

        let inactive = rows
            .iter()
            .find(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .contains("jp-02")
            })
            .expect("inactive node row");
        let name = inactive
            .iter()
            .position(|cell| cell.symbol() == "j")
            .expect("inactive name");
        assert_ne!(inactive[name].fg, Color::Cyan);
    }

    #[tokio::test]
    async fn subscriptions_keep_timestamp_when_some_nodes_are_unsupported() {
        let (app, _dir) = test_app(Lang::En).await;
        let mut view = app.view();
        view.page = Page::Subscriptions;
        view.subscriptions = vec![crate::view::SubscriptionView {
            name: "example".to_string(),
            url: "https://example.com/sub".to_string(),
            enabled: true,
            node_count: 8,
            unsupported_count: 2,
            last_updated: Some("2026-10-31 12:34:56".to_string()),
            error: None,
            stale: false,
        }];
        let content: String = rendered_rows(&view, 100, 30)
            .iter()
            .flatten()
            .map(String::as_str)
            .collect();
        assert!(content.contains("10-31 12:34:56"), "{content}");
        assert!(content.contains("8 (+2)"), "{content}");
        assert!(!content.contains("unsupported"), "{content}");

        // Refresh errors still take the status column.
        view.subscriptions[0].error = Some("HTTP 500".to_string());
        let content: String = rendered_rows(&view, 100, 30)
            .iter()
            .flatten()
            .map(String::as_str)
            .collect();
        assert!(
            content.contains("HTTP500") || content.contains("HTTP 500"),
            "{content}"
        );
    }

    #[tokio::test]
    async fn read_only_settings_and_dashboard_labels_are_not_dimmed() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        let backend = TestBackend::new(100, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &app.view())).unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(100).collect();
        let port_row = rows
            .iter()
            .find(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect::<String>()
                    .contains("端口")
            })
            .expect("port row");
        let port_label = port_row
            .iter()
            .find(|cell| cell.symbol() == "端")
            .expect("port label");
        assert_eq!(port_label.fg, Color::Reset);

        app.page = Page::Dashboard;
        terminal.draw(|frame| render(frame, &app.view())).unwrap();
        let cells = terminal.backend().buffer().content.clone();
        let rows: Vec<&[ratatui::buffer::Cell]> = cells.chunks(100).collect();
        let listen_row = rows
            .iter()
            .find(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect::<String>()
                    .contains("监听")
            })
            .expect("listen row");
        let listen_label = listen_row
            .iter()
            .find(|cell| cell.symbol() == "监")
            .expect("listen label");
        assert_eq!(listen_label.fg, Color::Reset);
    }

    #[tokio::test]
    async fn terminal_too_small_uses_the_rendered_minimum() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        let too_small = rendered_with_width(&app, 59, 15);
        assert!(too_small.contains("60"), "{too_small}");
        let minimum = rendered_with_width(&app, 60, 16);
        assert!(!minimum.contains("终端太小"), "{minimum}");
    }

    #[tokio::test]
    async fn small_terminal_renders_main_pages_without_panicking() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.nodes = vec![node("n1", "香港 01")];
        app.node_delays.insert("n1".to_string(), 68);
        app.metrics.connections.push(connection());
        for page in [
            Page::Dashboard,
            Page::Nodes,
            Page::Connections,
            Page::Logs,
            Page::Settings,
        ] {
            app.page = page;
            let content = rendered_with_width(&app, 60, 20);
            assert!(!content.is_empty(), "{page:?}");
        }
        app.overlay = Some(Overlay::Setup { selected: 0 });
        let content = rendered_with_width(&app, 60, 20);
        assert!(content.contains("DFRelay"), "{content}");
    }

    #[tokio::test]
    async fn footer_hides_list_navigation_hints() {
        let (app, _dir) = test_app(Lang::Zh).await;
        let content = rendered(&app, 30);
        let last_line: String = content.chars().skip(100 * 29).collect();
        assert!(!last_line.contains("h/l"), "{last_line}");
        assert!(!last_line.contains("j/k"), "{last_line}");
        let compact: String = last_line.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(compact.contains("q退出"), "{last_line}");
    }

    #[tokio::test]
    async fn renders_log_detail_overlay() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Logs;
        app.logger
            .app(LogLevel::Warn, "a long detail message for the popup");
        app.selected = 0;
        app.overlay = app
            .log_lines()
            .first()
            .cloned()
            .map(|line| Overlay::LogDetail { line, scroll: 0 });
        let content = rendered(&app, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("日志详情"), "{compact}");
        assert!(
            compact.contains("alongdetailmessageforthepopup"),
            "{compact}"
        );
    }

    #[tokio::test]
    async fn log_detail_scrolls_a_long_message() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Dashboard;
        let message = format!("start-marker {} tail-marker", "filler ".repeat(400));
        let line = LogLine {
            timestamp: "2026-11-01 12:00:00.000".to_string(),
            level: LogLevel::Info,
            component: crate::logging::Component::App,
            message,
        };
        app.overlay = Some(Overlay::LogDetail { line, scroll: 0 });

        let view = app.view();
        let max = log_detail_max_scroll(&view, 100, 30, Palette::Dark);
        assert!(max > 0, "long messages must be scrollable");
        let top = rendered(&app, 30);
        assert!(top.contains("start-marker"), "{top}");
        assert!(!top.contains("tail-marker"), "{top}");
        let top_compact: String = top.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(top_compact.contains("j/k滚动"), "{top_compact}");

        if let Some(Overlay::LogDetail { scroll, .. }) = app.overlay.as_mut() {
            *scroll = max;
        }
        let bottom = rendered(&app, 30);
        assert!(bottom.contains("tail-marker"), "{bottom}");
        assert!(!bottom.contains("start-marker"), "{bottom}");
        let bottom_compact: String = bottom.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(bottom_compact.contains("j/k滚动"), "{bottom_compact}");
    }

    #[tokio::test]
    async fn long_edit_form_feedback_does_not_clip_the_hint() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Settings;
        let mut form = KeywordForm::from_config(&app.config, KeywordField::Include);
        form.error = Some("word ".repeat(200));
        app.overlay = Some(Overlay::Edit(EditForm::Keyword(form)));
        let content = rendered_with_width(&app, 100, 30);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("Esc取消"), "{compact}");
    }

    #[tokio::test]
    async fn help_lists_global_and_page_sections() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Help;
        let content = rendered(&app, 40);
        let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("全局"), "{compact}");
        assert!(compact.contains("节点"), "{compact}");
        assert!(compact.contains("订阅"), "{compact}");
        assert!(compact.contains("日志"), "{compact}");
        let help: String = help_content_lines(&app.view(), 100)
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(help.contains("设置/sing-box"), "{help}");
        assert!(Lang::En
            .text()
            .help_sections
            .iter()
            .any(|section| section.title == "Settings/sing-box"));
    }

    #[tokio::test]
    async fn help_max_scroll_follows_the_viewport() {
        let (mut app, _dir) = test_app(Lang::Zh).await;
        app.page = Page::Help;
        let view = app.view();
        let small = help_max_scroll(&view, 80, 24);
        let large = help_max_scroll(&view, 80, 50);
        assert!(small > 0, "help content should overflow 80x24");
        assert!(large < small, "{large} should be less than {small}");
        assert_eq!(help_max_scroll(&view, 40, 10), 0);
    }
}
