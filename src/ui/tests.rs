#![allow(clippy::field_reassign_with_default)]
use super::*;
use crate::app::App;
use crate::config::{Config, Paths};
use crate::i18n::Lang;
use crate::logging::{LogLevel, LogTimestamp, Logger};
use crate::state::{
    settings_items, EditForm, KeywordField, KeywordForm, LineInput, Overlay, SettingsForm,
    SettingsItem, SubscriptionForm,
};
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
        let updated = find_cell_text(header, text.subscriptions_updated).expect("updated header");
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
        view.connection_count = 1;
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
        let duration = find_cell_text(header, text.connections_duration).expect("duration header");
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

fn render(frame: &mut Frame, view: &View) {
    render_at(frame, view, 0, Palette::Dark);
}
