#![allow(clippy::field_reassign_with_default)]
use super::health::probe_failure_outcome;
use super::subscriptions::PendingRefreshApply;
use super::*;
use crate::singbox::binary::InboxCandidate;
use crate::state::{KeywordField, SettingsField, SettingsItem};
use tempfile::TempDir;

async fn test_app() -> (App, TempDir) {
    let dir = TempDir::new().unwrap();
    let paths = Paths::discover(
        Some(dir.path().join("config.toml")),
        Some(dir.path().join("data")),
    )
    .unwrap();
    paths.ensure().unwrap();
    let config = Config::default();
    let logger = Logger::new(&paths.logs_dir, LogLevel::Info, LogLevel::Info).unwrap();
    let mut app = App::new(config, paths, logger).await;
    app.ui_state.overlay = None;
    (app, dir)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn setting_index(app: &App, item: SettingsItem) -> usize {
    settings_items(app.config.allow_lan)
        .iter()
        .position(|entry| *entry == item)
        .unwrap()
}

fn node(name: &str) -> Node {
    Node::new(
        name.to_string(),
        "test".to_string(),
        "shadowsocks".to_string(),
        "example.com".to_string(),
        443,
        serde_json::json!({
            "type": "shadowsocks", "server": "example.com", "server_port": 443,
            "method": "aes-128-gcm", "password": "secret"
        }),
    )
}

#[tokio::test]
async fn hjkl_navigates_pages_and_lists() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("one"), node("two"), node("three")];
    app.ui_state.page = Page::Nodes;

    app.handle_key(key(KeyCode::Char('j'))).await;
    assert_eq!(app.ui_state.selected, 1);
    app.handle_key(key(KeyCode::Char('k'))).await;
    assert_eq!(app.ui_state.selected, 0);
    app.handle_key(key(KeyCode::Char('G'))).await;
    assert_eq!(app.ui_state.selected, 2);
    app.handle_key(key(KeyCode::Char('g'))).await;
    assert_eq!(app.ui_state.selected, 0);

    app.handle_key(key(KeyCode::Char('l'))).await;
    assert_eq!(app.ui_state.page, Page::Subscriptions);
    app.handle_key(key(KeyCode::Char('h'))).await;
    assert_eq!(app.ui_state.page, Page::Nodes);
    app.handle_key(key(KeyCode::Right)).await;
    assert_eq!(app.ui_state.page, Page::Subscriptions);
    app.handle_key(key(KeyCode::Left)).await;
    assert_eq!(app.ui_state.page, Page::Nodes);
}

#[tokio::test]
async fn home_and_end_jump_like_g_and_big_g() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("one"), node("two"), node("three")];
    app.ui_state.page = Page::Nodes;
    app.handle_key(key(KeyCode::End)).await;
    assert_eq!(app.ui_state.selected, 2);
    app.handle_key(key(KeyCode::Home)).await;
    assert_eq!(app.ui_state.selected, 0);

    app.ui_state.page = Page::Dashboard;
    app.ui_state.dashboard_max_scroll = 4;
    app.handle_key(key(KeyCode::End)).await;
    assert_eq!(app.ui_state.dashboard_scroll, 4);
    app.handle_key(key(KeyCode::Home)).await;
    assert_eq!(app.ui_state.dashboard_scroll, 0);

    app.ui_state.page = Page::Help;
    app.ui_state.help_max_scroll = 3;
    app.handle_key(key(KeyCode::End)).await;
    assert_eq!(app.ui_state.help_scroll, 3);
    app.handle_key(key(KeyCode::Home)).await;
    assert_eq!(app.ui_state.help_scroll, 0);
}

#[tokio::test]
async fn confirmation_keys_only_cancel() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("one"), node("two")];
    app.ui_state.page = Page::Nodes;
    app.ui_state.selected = 0;

    app.proxy_runtime.status = ProxyStatus::Running;
    app.handle_key(key(KeyCode::Char('R'))).await;
    assert!(app.ui_state.restart_confirm);
    app.handle_key(key(KeyCode::Char('j'))).await;
    assert!(!app.ui_state.restart_confirm);
    assert_eq!(app.ui_state.selected, 0);

    app.ui_state.page = Page::Subscriptions;
    let config = SubscriptionConfig {
        id: "s-1".to_string(),
        name: "test".to_string(),
        url: "https://example.com/sub".to_string(),
        enabled: false,
    };
    app.config.subscriptions.push(config.clone());
    app.subscription_state
        .subscriptions
        .push(empty_runtime(config));
    app.ui_state.selected = 0;
    app.handle_key(key(KeyCode::Char('d'))).await;
    assert!(app.ui_state.subscription_delete.is_some());
    app.handle_key(key(KeyCode::Char('n'))).await;
    assert!(app.ui_state.subscription_delete.is_none());
    assert!(app.ui_state.overlay.is_none());
}

#[tokio::test]
async fn ctrl_c_clears_other_confirmations() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.restart_confirm = true;
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await;
    assert!(!app.ui_state.restart_confirm);
    assert!(!app.should_quit);

    app.ui_state.subscription_delete = Some("s-1".to_string());
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await;
    assert!(app.ui_state.subscription_delete.is_none());
    assert!(!app.should_quit);
}

#[tokio::test]
async fn start_stop_reports_busy_state() {
    let (mut app, _dir) = test_app().await;
    app.proxy_runtime.status = ProxyStatus::Starting;
    app.handle_key(key(KeyCode::Char('s'))).await;
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(app.text().msg_proxy_busy)
    );
}

#[tokio::test]
async fn dashboard_enter_opens_logs_only_when_an_error_is_shown() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Dashboard;
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(app.ui_state.page, Page::Dashboard);

    app.ui_state.last_error = Some("boom".to_string());
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(app.ui_state.page, Page::Logs);
}

#[tokio::test]
async fn settings_install_entry_opens_setup_popup() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Settings;
    app.ui_state.selected = setting_index(&app, SettingsItem::BinaryInfo);
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(app.ui_state.page, Page::Settings);
    assert!(matches!(
        app.ui_state.overlay,
        Some(Overlay::Setup { selected: 0 })
    ));
    assert_eq!(app.view().tab_page, Page::Settings);

    app.handle_key(key(KeyCode::Esc)).await;
    assert_eq!(app.ui_state.page, Page::Settings);
    assert!(app.ui_state.overlay.is_none());
    assert_eq!(app.view().tab_page, Page::Settings);
}

#[tokio::test]
async fn missing_binary_opens_the_setup_popup_on_startup() {
    let dir = TempDir::new().unwrap();
    let paths = Paths::discover(
        Some(dir.path().join("config.toml")),
        Some(dir.path().join("data")),
    )
    .unwrap();
    paths.ensure().unwrap();
    let logger = Logger::new(&paths.logs_dir, LogLevel::Info, LogLevel::Info).unwrap();
    let app = App::new(Config::default(), paths, logger).await;
    assert_eq!(app.ui_state.page, Page::Dashboard);
    assert!(matches!(
        app.ui_state.overlay,
        Some(Overlay::Setup { selected: 0 })
    ));
}

#[tokio::test]
async fn binary_status_tracks_install_and_upgrade() {
    let (mut app, _dir) = test_app().await;
    assert_eq!(app.view().binary_status, BinaryStatus::NotInstalled);

    app.binary_installed = true;
    app.binary_version = Some("1.14.4".to_string());
    assert_eq!(app.view().binary_status, BinaryStatus::Installed);

    app.latest_binary_version = Some("1.15.0".to_string());
    assert_eq!(app.view().binary_status, BinaryStatus::UpgradeAvailable);

    app.latest_binary_version = Some("1.14.4".to_string());
    assert_eq!(app.view().binary_status, BinaryStatus::Installed);

    app.latest_binary_version = Some("1.14.4-alpha.13".to_string());
    assert_eq!(app.view().binary_status, BinaryStatus::Installed);

    app.handle_event(AppEvent::UpdateCheckDone(Ok("1.16.0".to_string())))
        .await;
    assert!(!app.update_check_in_flight);
    assert_eq!(app.latest_binary_version.as_deref(), Some("1.16.0"));
    assert_eq!(app.view().binary_status, BinaryStatus::UpgradeAvailable);

    app.handle_event(AppEvent::UpdateCheckDone(Err("offline".to_string())))
        .await;
    assert!(!app.update_check_in_flight);
    assert_eq!(app.latest_binary_version.as_deref(), Some("1.16.0"));
}

#[tokio::test]
async fn rollback_requires_previous_binary() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.overlay = Some(Overlay::Setup { selected: 0 });
    app.handle_key(key(KeyCode::Char('b'))).await;
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(app.text().msg_rollback_unavailable)
    );
    assert!(!app.ui_state.rollback_confirm);
}

#[tokio::test]
async fn rollback_confirmation_is_consumed_by_other_keys() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.overlay = Some(Overlay::Setup { selected: 0 });
    std::fs::write(app.paths.previous_binary(), b"previous").unwrap();
    app.handle_key(key(KeyCode::Char('b'))).await;
    assert!(app.ui_state.rollback_confirm);
    app.handle_key(key(KeyCode::Char('j'))).await;
    assert!(!app.ui_state.rollback_confirm);
}

#[tokio::test]
async fn unverified_install_confirmation_is_cancelled_by_other_keys() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.overlay = Some(Overlay::Setup { selected: 0 });
    app.setup.candidates = vec![InboxCandidate {
        path: PathBuf::from("/inbox/raw-sing-box"),
        label: "raw-sing-box".to_string(),
        has_checksum: false,
    }];
    let expected = PathBuf::from("/inbox/raw-sing-box");
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(app.setup.pending_install.as_ref(), Some(&expected));
    assert!(app.setup.message.is_none());
    assert!(!app.should_quit);

    app.handle_key(key(KeyCode::Char('j'))).await;
    assert!(app.setup.pending_install.is_none());
    assert!(matches!(app.ui_state.overlay, Some(Overlay::Setup { .. })));

    app.handle_key(key(KeyCode::Enter)).await;
    assert!(app.setup.pending_install.is_some());
    app.handle_key(key(KeyCode::Char('q'))).await;
    assert!(app.setup.pending_install.is_none());
    assert!(!app.should_quit);

    app.handle_key(key(KeyCode::Enter)).await;
    assert!(app.setup.pending_install.is_some());
    app.handle_key(key(KeyCode::Esc)).await;
    assert!(app.setup.pending_install.is_none());
    assert!(matches!(app.ui_state.overlay, Some(Overlay::Setup { .. })));
}

#[tokio::test]
async fn ctrl_c_clears_unverified_install_confirmation() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.overlay = Some(Overlay::Setup { selected: 0 });
    app.setup.pending_install = Some(PathBuf::from("/inbox/raw-sing-box"));
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await;
    assert!(app.setup.pending_install.is_none());
    assert!(!app.should_quit);
}

#[tokio::test]
async fn rollback_done_updates_the_current_version() {
    let (mut app, _dir) = test_app().await;
    app.binary_installed = true;
    app.binary_version = Some("1.15.0".to_string());
    app.previous_binary_available = true;
    app.previous_binary_version = Some("1.14.4".to_string());
    app.setup.rollback_in_flight = true;
    let metadata = BinaryMetadata {
        version: "1.14.4".to_string(),
        source: "test".to_string(),
        sha256: None,
        installed_at: "2026-11-01 00:00:00".to_string(),
        verified: true,
    };
    app.handle_event(AppEvent::RollbackDone(Ok(metadata))).await;
    assert_eq!(app.binary_version.as_deref(), Some("1.14.4"));
    assert!(!app.setup.rollback_in_flight);
    assert!(app
        .ui_state
        .message
        .as_deref()
        .unwrap_or_default()
        .contains(app.text().msg_binary_switched));
}

#[tokio::test]
async fn subscription_refresh_clears_pending_auto_restart() {
    let (mut app, _dir) = test_app().await;
    let config = SubscriptionConfig {
        id: "s-1".to_string(),
        name: "test".to_string(),
        url: "https://example.com/sub".to_string(),
        enabled: true,
    };
    app.config.subscriptions.push(config.clone());
    app.subscription_state
        .subscriptions
        .push(empty_runtime(config.clone()));
    app.subscription_state
        .subscription_refresh_seq
        .insert(config.id.clone(), 7);
    app.subscription_state.pending_restart_subscription = Some(config.id.clone());
    app.handle_event(AppEvent::SubscriptionUpdated {
        seq: 7,
        runtime: empty_runtime(config),
    })
    .await;
    assert!(app
        .subscription_state
        .pending_restart_subscription
        .is_none());
}

#[tokio::test]
async fn unregistered_control_requests_are_ignored() {
    let (mut app, _dir) = test_app().await;
    app.handle_event(AppEvent::ShutdownRequest { client_id: 99 })
        .await;
    assert!(!app.should_quit);
    app.handle_event(AppEvent::StartProxyRequest { client_id: 99 })
        .await;
    assert_eq!(app.proxy_runtime.status, ProxyStatus::Stopped);
}

#[tokio::test]
async fn background_panic_clears_related_in_flight_state() {
    let (mut app, _dir) = test_app().await;
    app.subscription_state.refreshing_subscriptions = true;
    app.health.recovery_refresh_pending = true;
    app.handle_event(AppEvent::BackgroundPanic {
        task: BackgroundTask::SubscriptionRefresh,
        message: "boom".into(),
    })
    .await;
    assert!(!app.subscription_state.refreshing_subscriptions);
    assert!(!app.health.recovery_refresh_pending);

    app.subscription_state.pending_restart_subscription = Some("sub".into());
    app.handle_event(AppEvent::BackgroundPanic {
        task: BackgroundTask::Other,
        message: "boom".into(),
    })
    .await;
    assert!(app
        .subscription_state
        .pending_restart_subscription
        .is_none());
}

#[tokio::test]
async fn restart_in_progress_blocks_keys_except_detach() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("one"), node("two")];
    app.ui_state.page = Page::Nodes;
    app.ui_state.selected = 0;
    app.proxy_runtime.restart_in_progress = true;
    let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
    app.handle_event(AppEvent::ClientAttached {
        id: 11,
        viewer: true,
        tx,
        accept: tokio::sync::oneshot::channel().0,
    })
    .await;

    app.handle_key(key(KeyCode::Char('j'))).await;
    assert_eq!(app.ui_state.selected, 0);
    app.handle_key(key(KeyCode::Char('l'))).await;
    assert_eq!(app.ui_state.page, Page::Nodes);
    app.handle_key(key(KeyCode::Char('q'))).await;
    assert!(!app.should_quit);
    assert!(!app.has_viewer());
    assert!(drain_until_detach(&mut rx));
    app.handle_key(key(KeyCode::Char('s'))).await;
    assert_eq!(app.proxy_runtime.status, ProxyStatus::Stopped);
}

#[tokio::test]
async fn connections_enter_copies_the_selected_target() {
    let (mut app, _dir) = test_app().await;
    let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
    app.handle_event(AppEvent::ClientAttached {
        id: 1,
        viewer: true,
        tx,
        accept: tokio::sync::oneshot::channel().0,
    })
    .await;
    app.ui_state.page = Page::Connections;
    app.metrics
        .connections
        .push(crate::singbox::clash::ConnectionInfo {
            id: "id1".to_string(),
            host: "example.com".to_string(),
            destination_ip: "203.0.113.5".to_string(),
            destination_port: "443".to_string(),
            network: "tcp".to_string(),
            upload: 1,
            download: 2,
            duration_secs: None,
        });
    app.ui_state.selected = 0;
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(app.text().settings_copied)
    );
    let mut copied = None;
    while let Ok(message) = rx.try_recv() {
        if let ServerMsg::Copy { text } = message {
            copied = Some(text);
        }
    }
    assert_eq!(copied.as_deref(), Some("example.com:443"));
}

#[tokio::test]
async fn setup_copies_the_binary_path() {
    let (mut app, _dir) = test_app().await;
    let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
    app.handle_event(AppEvent::ClientAttached {
        id: 1,
        viewer: true,
        tx,
        accept: tokio::sync::oneshot::channel().0,
    })
    .await;
    app.ui_state.overlay = Some(Overlay::Setup { selected: 0 });
    let expected = app.paths.binary().display().to_string();
    app.handle_key(key(KeyCode::Char('p'))).await;
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(app.text().settings_copied)
    );
    let mut copied = None;
    while let Ok(message) = rx.try_recv() {
        if let ServerMsg::Copy { text } = message {
            copied = Some(text);
        }
    }
    assert_eq!(copied.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn logs_follow_tail_until_scrolled() {
    let (mut app, _dir) = test_app().await;
    for index in 0..20 {
        app.logger.app(LogLevel::Info, format!("line {index}"));
    }
    app.ui_state.page = Page::Logs;
    app.ui_state.log_follow = true;
    app.ui_state.selected = app.max_selection().saturating_sub(1);
    assert_eq!(app.ui_state.selected, 19);

    app.handle_key(key(KeyCode::Char('k'))).await;
    assert!(!app.ui_state.log_follow);
    assert_eq!(app.ui_state.selected, 18);
    app.handle_key(key(KeyCode::Char('j'))).await;
    assert!(app.ui_state.log_follow);
    assert_eq!(app.ui_state.selected, 19);
    app.handle_key(key(KeyCode::Char('k'))).await;
    assert!(!app.ui_state.log_follow);
    assert_eq!(app.ui_state.selected, 18);
    app.handle_key(key(KeyCode::Char('G'))).await;
    assert!(app.ui_state.log_follow);
    assert_eq!(app.ui_state.selected, 19);
    app.handle_key(key(KeyCode::Char('f'))).await;
    assert!(!app.ui_state.log_follow);
}

#[tokio::test]
async fn logs_source_cycles_and_filters_lines() {
    let (mut app, _dir) = test_app().await;
    app.logger.app(LogLevel::Info, "app line");
    app.logger.singbox(LogLevel::Info, "sing-box line");
    app.ui_state.page = Page::Logs;
    app.ui_state.log_follow = true;
    app.ui_state.selected = app.max_selection().saturating_sub(1);

    assert_eq!(app.ui_state.log_source, LogSource::All);
    assert_eq!(app.log_lines().len(), 2);

    app.handle_key(key(KeyCode::Char('x'))).await;
    assert_eq!(app.ui_state.log_source, LogSource::App);
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(format!("{}: app", app.text().logs_source).as_str())
    );
    assert_eq!(app.log_lines().len(), 1);
    assert_eq!(app.log_lines()[0].message, "app line");
    assert_eq!(app.ui_state.selected, 0);

    app.handle_key(key(KeyCode::Char('x'))).await;
    assert_eq!(app.ui_state.log_source, LogSource::Singbox);
    assert_eq!(app.log_lines().len(), 1);
    assert_eq!(app.log_lines()[0].message, "sing-box line");

    app.handle_key(key(KeyCode::Char('x'))).await;
    assert_eq!(app.ui_state.log_source, LogSource::All);
    assert_eq!(app.log_lines().len(), 2);
}

#[tokio::test]
async fn nodes_sort_by_auto_candidates_and_latency_then_name() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("Alpha"), node("Beta"), node("Gamma"), node("Delta")];
    for item in &mut app.nodes {
        item.tag = format!("tag-{}", item.name);
    }
    app.config.keywords_include = vec!["Alpha".to_string(), "Beta".to_string()];
    for item in &app.nodes {
        let delay = match item.name.as_str() {
            "Alpha" => 100,
            "Beta" => 20,
            "Delta" => 50,
            _ => 0,
        };
        if delay > 0 {
            app.node_delays.insert(item.tag.clone(), delay);
        }
    }
    app.ui_state.page = Page::Nodes;
    app.ui_state.selected = app
        .nodes
        .iter()
        .position(|item| item.name == "Delta")
        .unwrap();
    app.sort_nodes_preserving_selection();
    assert_eq!(
        app.nodes
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Beta", "Alpha", "Delta", "Gamma"]
    );
    assert_eq!(app.nodes[app.ui_state.selected].name, "Delta");

    app.handle_key(key(KeyCode::Char('n'))).await;
    assert_eq!(
        app.nodes
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Alpha", "Beta", "Delta", "Gamma"]
    );
    assert_eq!(app.nodes[app.ui_state.selected].name, "Delta");

    app.handle_key(key(KeyCode::Char('n'))).await;
    assert_eq!(
        app.nodes
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Beta", "Alpha", "Delta", "Gamma"]
    );
}

#[tokio::test]
async fn restart_requires_two_presses_and_can_be_cancelled() {
    let (mut app, _dir) = test_app().await;
    app.proxy_runtime.status = ProxyStatus::Running;
    app.handle_key(key(KeyCode::Char('R'))).await;
    assert!(app.ui_state.restart_confirm);
    assert!(app.view().restart_confirm);
    app.handle_key(key(KeyCode::Esc)).await;
    assert!(!app.ui_state.restart_confirm);

    app.proxy_runtime.status = ProxyStatus::Stopped;
    app.handle_key(key(KeyCode::Char('R'))).await;
    assert!(!app.ui_state.restart_confirm);
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(app.text().msg_proxy_not_running)
    );
}

#[tokio::test]
async fn logs_timestamp_cycles_through_time_date_and_hidden() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Logs;
    assert_eq!(app.ui_state.log_timestamp, LogTimestamp::Time);
    app.handle_key(key(KeyCode::Char('d'))).await;
    assert_eq!(app.ui_state.log_timestamp, LogTimestamp::DateTime);
    app.handle_key(key(KeyCode::Char('d'))).await;
    assert_eq!(app.ui_state.log_timestamp, LogTimestamp::Hidden);
    app.handle_key(key(KeyCode::Char('d'))).await;
    assert_eq!(app.ui_state.log_timestamp, LogTimestamp::Time);
}

#[tokio::test]
async fn modified_shortcuts_are_ignored() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Nodes;
    app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL))
        .await;
    assert_eq!(app.ui_state.page, Page::Nodes);
    app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT))
        .await;
    assert!(app.ui_state.message.is_none());
}

#[test]
fn listen_addresses_use_the_actual_ipv6_binding() {
    assert_eq!(
        format_listen_addresses("127.0.0.1", 12000, true),
        "127.0.0.1:12000 + [::1]:12000"
    );
    assert_eq!(
        format_listen_addresses("0.0.0.0", 12000, false),
        "0.0.0.0:12000"
    );
}

#[tokio::test]
async fn stale_subscription_refresh_results_are_ignored() {
    let (mut app, _dir) = test_app().await;
    let config = SubscriptionConfig {
        id: "sub".to_string(),
        name: "sub".to_string(),
        url: "https://example.com/sub".to_string(),
        enabled: true,
    };
    app.config.subscriptions = vec![config.clone()];
    app.subscription_state.subscription_seq = 2;
    app.subscription_state
        .subscription_refresh_seq
        .insert(config.id.clone(), app.subscription_state.subscription_seq);

    let mut stale = empty_runtime(config.clone());
    stale.nodes = vec![node("stale")];
    app.handle_event(AppEvent::SubscriptionUpdated {
        seq: 1,
        runtime: stale,
    })
    .await;
    assert!(app.subscription_state.subscriptions.is_empty());

    let mut fresh = empty_runtime(config);
    fresh.nodes = vec![node("fresh")];
    app.handle_event(AppEvent::SubscriptionUpdated {
        seq: app.subscription_state.subscription_seq,
        runtime: fresh,
    })
    .await;
    assert_eq!(app.subscription_state.subscriptions.len(), 1);
    assert_eq!(app.nodes[0].name, "fresh");
}

#[tokio::test]
async fn config_save_failure_keeps_the_in_memory_config() {
    let (mut app, _dir) = test_app().await;
    let _ = std::fs::remove_file(&app.paths.config_file);
    std::fs::create_dir(&app.paths.config_file).unwrap();
    let language = app.config.language;
    app.toggle_language();
    assert_eq!(app.config.language, language);
    assert!(app.ui_state.last_error.is_some());
}

#[tokio::test]
async fn subscription_errors_are_not_cleared_by_another_success() {
    let (mut app, _dir) = test_app().await;
    let config_a = SubscriptionConfig {
        id: "a".to_string(),
        name: "a".to_string(),
        url: "https://example.com/a".to_string(),
        enabled: true,
    };
    let config_b = SubscriptionConfig {
        id: "b".to_string(),
        name: "b".to_string(),
        url: "https://example.com/b".to_string(),
        enabled: true,
    };
    app.config.subscriptions = vec![config_a.clone(), config_b.clone()];
    let mut failed = empty_runtime(config_a);
    failed.error = Some("HTTP 500".to_string());
    app.subscription_state.subscriptions = vec![failed, empty_runtime(config_b.clone())];
    app.refresh_last_error_from_subscriptions();
    assert!(app
        .ui_state
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("a: HTTP 500")));

    let seq = app.next_subscription_seq();
    app.subscription_state
        .subscription_refresh_seq
        .insert(config_b.id.clone(), seq);
    app.handle_event(AppEvent::SubscriptionUpdated {
        seq,
        runtime: empty_runtime(config_b),
    })
    .await;
    assert!(app
        .ui_state
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("a: HTTP 500")));
}

#[tokio::test]
async fn lan_access_defaults_off_and_toggle_warns() {
    let (mut app, _dir) = test_app().await;
    assert!(!app.config.allow_lan);
    assert!(app.listen_addresses().starts_with("127.0.0.1"));

    app.toggle_allow_lan();
    assert!(app.config.allow_lan);
    assert!(app.listen_addresses().starts_with("0.0.0.0"));
    assert!(app
        .ui_state
        .message
        .as_deref()
        .unwrap_or_default()
        .contains(app.text().settings_lan_warning));

    app.toggle_allow_lan();
    assert!(!app.config.allow_lan);
}

#[tokio::test]
async fn settings_jk_selects_and_enter_toggles_or_edits() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Settings;
    app.ui_state.selected = setting_index(&app, SettingsItem::Language);
    let language = app.config.language;
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(app.config.language, language.toggle());

    app.ui_state.selected = setting_index(&app, SettingsItem::AllowLan);
    app.handle_key(key(KeyCode::Enter)).await;
    assert!(app.config.allow_lan);

    app.handle_key(key(KeyCode::Char('j'))).await;
    assert_eq!(
        app.ui_state.selected,
        setting_index(&app, SettingsItem::LanAuth)
    );
    app.handle_key(key(KeyCode::Char('k'))).await;
    assert_eq!(
        app.ui_state.selected,
        setting_index(&app, SettingsItem::AllowLan)
    );

    app.ui_state.selected = setting_index(&app, SettingsItem::KeywordsExclude);
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(
        app.keyword_form().map(|form| form.field),
        Some(KeywordField::Exclude)
    );
}

#[tokio::test]
async fn test_url_is_read_only_and_copyable() {
    let (mut app, _dir) = test_app().await;
    let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
    app.handle_event(AppEvent::ClientAttached {
        id: 1,
        viewer: true,
        tx,
        accept: tokio::sync::oneshot::channel().0,
    })
    .await;
    app.ui_state.page = Page::Settings;
    app.ui_state.selected = setting_index(&app, SettingsItem::TestUrl);
    let expected = app.config.test_url.clone();
    app.handle_key(key(KeyCode::Enter)).await;
    assert!(app.settings_form().is_none());
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(app.text().settings_copied)
    );
    let mut copied = None;
    while let Ok(message) = rx.try_recv() {
        if let ServerMsg::Copy { text } = message {
            copied = Some(text);
        }
    }
    assert_eq!(copied.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn settings_read_only_item_copies_its_value() {
    let (mut app, _dir) = test_app().await;
    let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
    app.handle_event(AppEvent::ClientAttached {
        id: 1,
        viewer: true,
        tx,
        accept: tokio::sync::oneshot::channel().0,
    })
    .await;
    app.ui_state.page = Page::Settings;
    app.ui_state.selected = setting_index(&app, SettingsItem::ConfigPath);
    let expected = app.paths.config_file.display().to_string();
    app.handle_key(key(KeyCode::Enter)).await;
    assert!(app.settings_form().is_none());
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(app.text().settings_copied)
    );
    let mut copied = None;
    while let Ok(message) = rx.try_recv() {
        if let ServerMsg::Copy { text } = message {
            copied = Some(text);
        }
    }
    assert_eq!(copied.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn port_and_dns_are_read_only_and_copyable() {
    let (mut app, _dir) = test_app().await;
    let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
    app.handle_event(AppEvent::ClientAttached {
        id: 1,
        viewer: true,
        tx,
        accept: tokio::sync::oneshot::channel().0,
    })
    .await;
    app.ui_state.page = Page::Settings;
    for (item, expected) in [
        (SettingsItem::Port, "12000".to_string()),
        (SettingsItem::DnsServers, "1.1.1.1, 8.8.8.8".to_string()),
    ] {
        app.ui_state.selected = setting_index(&app, item);
        app.handle_key(key(KeyCode::Enter)).await;
        assert!(app.settings_form().is_none());
        let mut copied = None;
        while let Ok(message) = rx.try_recv() {
            if let ServerMsg::Copy { text } = message {
                copied = Some(text);
            }
        }
        assert_eq!(copied, Some(expected));
    }
}

#[tokio::test]
async fn lan_auth_is_one_setting_and_persists_both_fields() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Settings;
    let base = settings_items(false).len();
    assert_eq!(settings_items(true).len(), base + 1);
    assert!(!settings_items(false).contains(&SettingsItem::LanAuth));

    app.toggle_allow_lan();
    assert!(app.config.allow_lan);
    assert!(settings_items(true).contains(&SettingsItem::LanAuth));

    app.ui_state.selected = setting_index(&app, SettingsItem::LanAuth);
    app.handle_key(key(KeyCode::Enter)).await;
    let form = app.settings_form().expect("LAN auth form");
    assert_eq!(form.active, SettingsField::LanUsername);
    assert!(form.username.value.is_empty());
    assert!(form.password.value.is_empty());
    assert_eq!(form.title(app.text()), app.text().settings_lan_auth_title);

    if let Some(form) = app.settings_form_mut() {
        form.username = LineInput::new("alice");
    }
    app.handle_key(key(KeyCode::Tab)).await;
    assert_eq!(
        app.settings_form().unwrap().active,
        SettingsField::LanPassword
    );
    if let Some(form) = app.settings_form_mut() {
        form.password = LineInput::new("s3cret");
    }
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(app.config.lan_username, "alice");
    assert_eq!(app.config.lan_password, "s3cret");
    assert_eq!(app.config.lan_auth(), Some(("alice", "s3cret")));

    let (loaded, warnings) = Config::load_or_default(&app.paths.config_file);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(loaded.lan_auth(), Some(("alice", "s3cret")));

    let view = app.view();
    assert!(view.lan_password_set);
    assert!(view.lan_auth_configured());
    let encoded = serde_json::to_string(&view).unwrap();
    assert!(!encoded.contains("s3cret"));
    assert!(!encoded.contains("\"lan_password\":"));
}

#[tokio::test]
async fn lan_auth_form_keeps_password_visible_and_ignores_ctrl_r() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Settings;
    app.config.allow_lan = true;
    app.config.lan_username = "alice".into();
    app.config.lan_password = "s3cret".into();
    app.ui_state.selected = setting_index(&app, SettingsItem::LanAuth);
    app.handle_key(key(KeyCode::Enter)).await;
    let form = app.settings_form().expect("LAN auth form");
    assert_eq!(form.username.value, "alice");
    assert_eq!(form.password.value, "s3cret");
    app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
        .await;
    assert_eq!(app.settings_form().unwrap().password.value, "s3cret");
    assert_eq!(
        app.settings_form().unwrap().active,
        SettingsField::LanUsername
    );
}

#[tokio::test]
async fn singbox_log_level_is_read_only_on_setup() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.overlay = Some(Overlay::Setup { selected: 0 });
    let before = app.config.singbox_log_level;
    app.handle_key(key(KeyCode::Char('v'))).await;
    assert_eq!(app.config.singbox_log_level, before);
    let (loaded, _) = Config::load_or_default(&app.paths.config_file);
    assert_eq!(loaded.singbox_log_level, before);
}

#[tokio::test]
async fn q_detaches_viewers_without_stopping_daemon() {
    let (mut app, _dir) = test_app().await;
    let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
    app.handle_event(AppEvent::ClientAttached {
        id: 7,
        viewer: true,
        tx,
        accept: tokio::sync::oneshot::channel().0,
    })
    .await;
    app.handle_key(key(KeyCode::Char('q'))).await;
    assert!(drain_until_detach(&mut rx));
    assert!(!app.should_quit);
    assert!(app.clients.is_empty());
    assert!(!app.has_viewer());

    let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
    app.handle_event(AppEvent::ClientAttached {
        id: 8,
        viewer: true,
        tx,
        accept: tokio::sync::oneshot::channel().0,
    })
    .await;
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await;
    assert!(drain_until_detach(&mut rx));
    assert!(!app.should_quit);
    assert!(app.clients.is_empty());
    assert!(!app.has_viewer());
}

fn drain_until_detach(rx: &mut mpsc::Receiver<ServerMsg>) -> bool {
    while let Ok(message) = rx.try_recv() {
        if matches!(message, ServerMsg::Detach) {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn status_request_reports_mode_and_proxy_state() {
    let (mut app, _dir) = test_app().await;
    app.proxy_runtime.status = ProxyStatus::Running;
    app.managed = true;
    app.service = Some(crate::service::ServiceIdentity {
        manager: crate::service::ServiceManager::SystemdUser,
        unit: "dfrelay.service".into(),
        target: None,
        uid: crate::service::current_uid(),
    });
    let (tx, mut rx) = mpsc::channel(VIEW_QUEUE_LIMIT);
    app.handle_event(AppEvent::ClientAttached {
        id: 9,
        viewer: false,
        tx,
        accept: tokio::sync::oneshot::channel().0,
    })
    .await;
    app.handle_event(AppEvent::StatusRequest { client_id: 9 })
        .await;
    let Ok(ServerMsg::Status(status)) = rx.try_recv() else {
        panic!("expected status response");
    };
    assert!(status.managed);
    assert_eq!(status.proxy, ProxyStatus::Running);
    assert_eq!(status.pid, std::process::id());
    assert_eq!(
        status.service.as_ref().map(|service| service.unit.as_str()),
        Some("dfrelay.service")
    );
}

#[tokio::test]
async fn restart_backoff_pauses_after_five_failures() {
    let (mut app, _dir) = test_app().await;
    for _ in 0..MAX_RESTART_ATTEMPTS {
        app.schedule_restart();
        assert!(app.proxy_runtime.restart_at.is_some());
    }
    assert_eq!(app.proxy_runtime.restart_attempts, MAX_RESTART_ATTEMPTS);
    app.schedule_restart();
    assert!(app.proxy_runtime.restart_at.is_none());
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(app.text().msg_restart_paused)
    );
}

#[tokio::test]
async fn subscription_form_validates_and_saves_disabled_entry() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Subscriptions;
    app.ui_state.overlay = Some(Overlay::Edit(EditForm::Subscription(
        SubscriptionForm::new(),
    )));
    if let Some(form) = app.subscription_form_mut() {
        form.url = LineInput::new("ftp://example.com/sub");
    }
    app.save_subscription_form();
    assert!(app.config.subscriptions.is_empty());
    assert!(app
        .subscription_form()
        .and_then(|form| form.error.as_ref())
        .is_some());

    if let Some(form) = app.subscription_form_mut() {
        form.url = LineInput::new("https://example.com/sub");
        form.enabled = false;
    }
    app.save_subscription_form();
    assert!(app.subscription_form().is_none());
    assert_eq!(app.config.subscriptions.len(), 1);
    assert!(!app.config.subscriptions[0].enabled);
    assert_eq!(app.subscription_state.subscriptions.len(), 1);
    assert!(app.config.subscriptions[0].name.contains("example.com"));

    let id = app.config.subscriptions[0].id.clone();
    app.delete_subscription(&id);
    assert!(app.config.subscriptions.is_empty());
    assert!(app.subscription_state.subscriptions.is_empty());
}

#[tokio::test]
async fn subscription_form_clears_its_error_when_the_user_edits() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Subscriptions;
    app.ui_state.overlay = Some(Overlay::Edit(EditForm::Subscription(
        SubscriptionForm::new(),
    )));
    if let Some(form) = app.subscription_form_mut() {
        form.url = LineInput::new("ftp://example.com/sub");
    }
    app.save_subscription_form();
    assert!(app.subscription_form().unwrap().error.is_some());

    app.handle_key(key(KeyCode::Char('x'))).await;
    assert!(app.subscription_form().unwrap().error.is_none());
}

#[tokio::test]
async fn subscription_delete_confirmation_is_persistent_and_cancellable() {
    let (mut app, _dir) = test_app().await;
    let config = SubscriptionConfig {
        id: "s-1".to_string(),
        name: "test".to_string(),
        url: "https://example.com/sub".to_string(),
        enabled: false,
    };
    app.config.subscriptions.push(config.clone());
    app.subscription_state
        .subscriptions
        .push(empty_runtime(config));
    app.ui_state.page = Page::Subscriptions;
    app.ui_state.selected = 0;

    app.handle_key(key(KeyCode::Char('d'))).await;
    assert!(app.ui_state.subscription_delete.is_some());
    assert!(app.view().subscription_delete);
    assert!(
        app.ui_state.message.is_none(),
        "confirmations must not use the toast"
    );

    app.ui_state.message = Some("expired toast".to_string());
    app.ui_state.message_at = Some(Instant::now() - TOAST_TTL - Duration::from_secs(1));
    app.on_tick().await;
    assert!(
        app.ui_state.subscription_delete.is_some(),
        "toast expiry must not cancel the confirmation"
    );

    app.handle_key(key(KeyCode::Char('j'))).await;
    assert!(app.ui_state.subscription_delete.is_none());

    app.handle_key(key(KeyCode::Char('d'))).await;
    app.handle_key(key(KeyCode::Esc)).await;
    assert!(app.ui_state.subscription_delete.is_none());
    assert_eq!(app.config.subscriptions.len(), 1);

    app.handle_key(key(KeyCode::Char('d'))).await;
    assert!(app.ui_state.subscription_delete.is_some());
    app.handle_key(key(KeyCode::Char('d'))).await;
    assert!(app.config.subscriptions.is_empty());
}

#[tokio::test]
async fn node_selection_while_stopped_applies_on_start() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("香港 01"), node("日本 02")];
    app.ui_state.page = Page::Nodes;
    app.ui_state.selected = 1;

    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(
        app.preferred_node.as_deref(),
        Some(app.nodes[1].tag.as_str())
    );
    assert_eq!(
        app.pending_selection,
        Some(PendingSelection::Node(app.nodes[1].tag.clone()))
    );
    assert!(app
        .ui_state
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("日本 02"));

    app.handle_key(key(KeyCode::Char('a'))).await;
    assert!(app.preferred_node.is_none());
    assert_eq!(app.pending_selection, Some(PendingSelection::Auto));
}

#[tokio::test]
async fn current_node_falls_back_to_name_when_stopped() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("香港 01")];
    app.preferred_node = Some(app.nodes[0].tag.clone());
    assert_eq!(app.current_node().as_deref(), Some("香港 01"));

    app.preferred_node = None;
    assert_eq!(app.current_node().as_deref(), Some(app.text().common_auto));
}

#[tokio::test]
async fn current_node_labels_removed_and_unknown_tags() {
    let (mut app, _dir) = test_app().await;
    app.nodes.clear();
    app.proxy_runtime
        .proxy_node_names
        .insert("n-old".to_string(), "香港 01".to_string());
    app.metrics.selection.selected = Some("n-old".to_string());
    assert_eq!(
        app.current_node(),
        Some(format!("香港 01{}", app.text().node_removed_suffix))
    );

    app.metrics.selection.selected = Some("n-missing".to_string());
    assert_eq!(app.current_node().as_deref(), Some(app.text().node_unknown));
}

#[tokio::test]
async fn selection_mode_follows_selector_state_and_pending_node() {
    let (mut app, _dir) = test_app().await;
    // Stopped without a pending node: automatic selection.
    assert!(app.selection_mode_auto());

    app.preferred_node = Some("n1".to_string());
    assert!(!app.selection_mode_auto());

    // While running the selector state is authoritative.
    app.metrics.selection.selected = Some("auto".to_string());
    assert!(app.selection_mode_auto());

    app.metrics.selection.selected = Some("n2".to_string());
    assert!(!app.selection_mode_auto());
}

#[tokio::test]
async fn keyword_form_saves_each_field_separately() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("香港 01"), node("日本 02")];
    app.config.keywords_exclude = vec!["过期".to_string()];
    let mut form = KeywordForm::from_config(&app.config, KeywordField::Include);
    form.value = LineInput::new("香港, HK");
    app.ui_state.overlay = Some(Overlay::Edit(EditForm::Keyword(form)));

    app.save_keyword_form();

    assert_eq!(app.config.keywords_include, vec!["香港", "HK"]);
    assert_eq!(app.config.keywords_exclude, vec!["过期"]);
    assert!(app.keyword_form().is_none());
    let message = app.ui_state.message.as_deref().unwrap_or_default();
    assert!(message.contains("1/2"), "{message}");

    let mut form = KeywordForm::from_config(&app.config, KeywordField::Exclude);
    form.value = LineInput::new("过期, 剩余");
    app.ui_state.overlay = Some(Overlay::Edit(EditForm::Keyword(form)));
    app.save_keyword_form();
    assert_eq!(app.config.keywords_include, vec!["香港", "HK"]);
    assert_eq!(app.config.keywords_exclude, vec!["过期", "剩余"]);
}

#[tokio::test]
async fn missing_preferred_node_is_cleared() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("香港 01")];
    app.proxy_runtime
        .proxy_node_names
        .insert("n-missing".to_string(), "旧节点".to_string());
    app.preferred_node = Some("n-missing".to_string());
    app.pending_selection = Some(PendingSelection::Node("n-missing".to_string()));
    app.sync_preferred_node();
    assert!(app.preferred_node.is_none());
    assert!(app.pending_selection.is_none());
    assert_eq!(
        app.ui_state.message.as_deref(),
        Some(app.text().msg_preferred_missing)
    );
    app.logger.flush();
    let logs = std::fs::read_to_string(app.logger.current_file()).unwrap();
    assert!(
        logs.contains("manual node removed from subscription: n-missing"),
        "{logs}"
    );
}

#[tokio::test]
async fn subscription_apply_tracks_tag_diff_and_clears_when_unchanged() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![tagged_node("A", "n1"), tagged_node("B", "n2")];
    app.proxy_runtime.proxy_node_tags = vec!["n1".to_string()];
    app.note_subscription_apply();
    let pending = app
        .subscription_state
        .pending_refresh_apply
        .as_ref()
        .expect("pending diff");
    assert_eq!((pending.added, pending.removed), (1, 0));

    app.nodes = vec![tagged_node("A", "n1")];
    app.note_subscription_apply();
    assert!(app.subscription_state.pending_refresh_apply.is_none());
}

#[tokio::test]
async fn unchanged_subscription_refresh_skips_the_restart() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![tagged_node("A", "n1")];
    app.proxy_runtime.proxy_node_tags = vec!["n1".to_string()];
    app.subscription_state.pending_restart_subscription = Some("s-1".to_string());
    app.restart_after_pending_subscription();
    assert!(app
        .subscription_state
        .pending_restart_subscription
        .is_none());
    assert!(!app.proxy_runtime.restart_in_progress);
}

#[tokio::test]
async fn reconcile_clears_the_pending_apply_after_a_matching_start() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![tagged_node("A", "n1")];
    app.proxy_runtime.proxy_node_tags = vec!["n1".to_string()];
    app.subscription_state.pending_refresh_apply = Some(PendingRefreshApply {
        added: 1,
        removed: 0,
        since: Instant::now(),
        wait_logged: false,
    });
    app.reconcile_subscription_apply_after_start();
    assert!(app.subscription_state.pending_refresh_apply.is_none());
}

#[tokio::test]
async fn pending_apply_is_dropped_when_the_proxy_is_not_running() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![tagged_node("A", "n1")];
    app.proxy_runtime.proxy_node_tags = vec!["n1".to_string(), "n2".to_string()];
    app.note_subscription_apply();
    assert!(app.subscription_state.pending_refresh_apply.is_some());
    app.maybe_apply_subscription_changes();
    assert!(app.subscription_state.pending_refresh_apply.is_none());
}

#[tokio::test]
async fn probe_targets_keep_the_primary_and_dedup_the_fallback() {
    let (mut app, _dir) = test_app().await;
    app.config.test_url = FALLBACK_TEST_URL.to_string();
    assert_eq!(app.probe_targets(), vec![FALLBACK_TEST_URL.to_string()]);

    app.config.test_url = "https://example.com/generate_204".to_string();
    assert_eq!(
        app.probe_targets(),
        vec![
            "https://example.com/generate_204".to_string(),
            FALLBACK_TEST_URL.to_string()
        ]
    );
}

#[tokio::test]
async fn switch_budget_pauses_health_failover_and_resets_the_window() {
    let (mut app, _dir) = test_app().await;
    for _ in 0..AUTO_SWITCH_BUDGET {
        app.record_auto_switch();
    }
    assert!(!app.switch_budget_available());
    assert!(app.health.auto_switch_times.is_empty());
    assert!(app.health.pause_until.is_some_and(|at| at > Instant::now()));
}

#[tokio::test]
async fn help_returns_to_the_previous_page() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Logs;

    app.handle_key(key(KeyCode::Char('?'))).await;
    assert_eq!(app.ui_state.page, Page::Help);
    assert_eq!(app.view().tab_page, Page::Logs);
    app.handle_key(key(KeyCode::Esc)).await;
    assert_eq!(app.ui_state.page, Page::Logs);

    app.handle_key(key(KeyCode::Char('?'))).await;
    assert_eq!(app.ui_state.page, Page::Help);
    app.handle_key(key(KeyCode::Char('?'))).await;
    assert_eq!(app.ui_state.page, Page::Logs);
}

#[tokio::test]
async fn help_scrolls_with_jk_and_clamps_at_bottom() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Logs;
    app.handle_key(key(KeyCode::Char('?'))).await;
    app.ui_state.help_max_scroll = 5;

    app.handle_key(key(KeyCode::Char('j'))).await;
    assert_eq!(app.ui_state.help_scroll, 1);
    app.handle_key(key(KeyCode::Down)).await;
    assert_eq!(app.ui_state.help_scroll, 2);
    app.handle_key(key(KeyCode::Char('k'))).await;
    assert_eq!(app.ui_state.help_scroll, 1);
    app.handle_key(key(KeyCode::Char('g'))).await;
    assert_eq!(app.ui_state.help_scroll, 0);
    app.handle_key(key(KeyCode::Char('G'))).await;
    assert_eq!(app.ui_state.help_scroll, 5);
    for _ in 0..10 {
        app.handle_key(key(KeyCode::Char('j'))).await;
    }
    assert_eq!(app.ui_state.help_scroll, 5);
    app.handle_key(key(KeyCode::Char('k'))).await;
    assert_eq!(app.ui_state.help_scroll, 4);

    app.handle_event(AppEvent::HelpViewport(2)).await;
    assert_eq!(app.ui_state.help_scroll, 2);
}

#[test]
fn line_input_moves_cursor_and_edits_at_position() {
    let mut input = LineInput::new("abcd");
    assert_eq!(input.cursor, 4);
    input.left();
    input.left();
    assert_eq!(input.cursor, 2);
    input.insert('X');
    assert_eq!(input.value, "abXcd");
    assert_eq!(input.cursor, 3);
    input.backspace();
    assert_eq!(input.value, "abcd");
    assert_eq!(input.cursor, 2);
    input.delete();
    assert_eq!(input.value, "abd");
    input.home();
    input.delete();
    assert_eq!(input.value, "bd");
    assert_eq!(input.cursor, 0);
    input.end();
    input.insert('尾');
    assert_eq!(input.value, "bd尾");
    assert_eq!(input.cursor, 3);
    input.left();
    input.insert('中');
    assert_eq!(input.value, "bd中尾");
}

#[tokio::test]
async fn enter_opens_log_detail_and_esc_closes_it() {
    let (mut app, _dir) = test_app().await;
    app.logger.app(LogLevel::Info, "a detailed log line");
    app.ui_state.page = Page::Logs;
    app.ui_state.log_follow = true;
    app.ui_state.selected = app.max_selection().saturating_sub(1);

    app.handle_key(key(KeyCode::Enter)).await;
    assert!(app.log_detail().is_some());
    app.handle_key(key(KeyCode::Esc)).await;
    assert!(app.log_detail().is_none());
}

#[tokio::test]
async fn log_detail_scrolls_and_clamps_to_the_viewport() {
    let (mut app, _dir) = test_app().await;
    for index in 0..20 {
        app.logger
            .app(LogLevel::Info, format!("scrolling line {index}"));
    }
    app.ui_state.page = Page::Logs;
    app.ui_state.log_follow = true;
    app.ui_state.selected = app.max_selection().saturating_sub(1);
    app.handle_key(key(KeyCode::Enter)).await;
    assert!(app.log_detail().is_some());

    app.handle_event(AppEvent::LogDetailViewport(5)).await;
    app.handle_key(key(KeyCode::Char('j'))).await;
    assert_eq!(app.log_detail_scroll(), 1);
    app.handle_key(key(KeyCode::Down)).await;
    assert_eq!(app.log_detail_scroll(), 2);
    app.handle_key(key(KeyCode::Char('k'))).await;
    assert_eq!(app.log_detail_scroll(), 1);
    app.handle_key(key(KeyCode::Char('g'))).await;
    assert_eq!(app.log_detail_scroll(), 0);
    app.handle_key(key(KeyCode::Char('G'))).await;
    assert_eq!(app.log_detail_scroll(), 5);
    app.handle_key(key(KeyCode::Char('j'))).await;
    assert_eq!(app.log_detail_scroll(), 5);
    app.handle_key(key(KeyCode::End)).await;
    assert_eq!(app.log_detail_scroll(), 5);
    app.handle_key(key(KeyCode::Char('k'))).await;
    assert_eq!(app.log_detail_scroll(), 4);
    app.handle_event(AppEvent::LogDetailViewport(2)).await;
    assert_eq!(app.log_detail_scroll(), 2);
    app.handle_key(key(KeyCode::Esc)).await;
    assert!(app.log_detail().is_none());
}

#[tokio::test]
async fn toast_expires_after_ttl() {
    let (mut app, _dir) = test_app().await;
    assert_eq!(TOAST_TTL, Duration::from_secs(3));
    app.notify("saved");
    assert!(app.view().toast.is_some());
    app.ui_state.message_at = Some(Instant::now() - TOAST_TTL - Duration::from_secs(1));
    assert!(app.view().toast.is_none());
}

#[tokio::test]
async fn dashboard_scroll_clamps_to_viewport() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Dashboard;
    app.handle_event(AppEvent::DashboardViewport(3)).await;
    for _ in 0..10 {
        app.handle_key(key(KeyCode::Char('j'))).await;
    }
    assert_eq!(app.ui_state.dashboard_scroll, 3);
    app.handle_key(key(KeyCode::Char('k'))).await;
    assert_eq!(app.ui_state.dashboard_scroll, 2);
    app.handle_key(key(KeyCode::Char('g'))).await;
    assert_eq!(app.ui_state.dashboard_scroll, 0);
    app.handle_key(key(KeyCode::Char('G'))).await;
    assert_eq!(app.ui_state.dashboard_scroll, 3);
    app.handle_event(AppEvent::DashboardViewport(1)).await;
    assert_eq!(app.ui_state.dashboard_scroll, 1);
}

#[tokio::test]
async fn view_roundtrips_through_json() {
    let (mut app, _dir) = test_app().await;
    app.node_delays.insert("n1".to_string(), 42);
    app.delay_testing.insert("n2".to_string());
    let encoded = serde_json::to_string(&app.view()).unwrap();
    let decoded: View = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.page, app.ui_state.page);
    assert_eq!(decoded.config.port, app.config.port);
    assert_eq!(decoded.node_delays.get("n1"), Some(&42));
    assert!(decoded.delay_testing.contains("n2"));
}

#[tokio::test]
async fn view_does_not_expose_secrets_or_unused_config_fields() {
    let (mut app, _dir) = test_app().await;
    app.ui_state.page = Page::Nodes;
    app.config.allow_lan = true;
    app.config.lan_password = "s3cret".to_string();
    app.config.subscriptions = vec![SubscriptionConfig {
        id: "s1".to_string(),
        name: "sub".to_string(),
        url: "https://example.com/path?token=secret".to_string(),
        enabled: true,
    }];
    app.nodes = vec![node("node")];

    let encoded = serde_json::to_string(&app.view()).unwrap();
    assert!(!encoded.contains("s3cret"), "{encoded}");
    assert!(!encoded.contains("token=secret"), "{encoded}");
    assert!(!encoded.contains("\"lan_password\":"), "{encoded}");
    assert!(!encoded.contains("\"password\":\"secret\""), "{encoded}");
    assert!(!encoded.contains("github_mirrors"), "{encoded}");
    assert!(!encoded.contains("proxy_ip_v4_url"), "{encoded}");
}

#[tokio::test]
async fn metrics_node_delays_feed_the_nodes_view() {
    let (mut app, _dir) = test_app().await;
    let mut metrics = MetricsSnapshot::default();
    metrics.node_delays.insert("n1".to_string(), 42);
    app.handle_event(AppEvent::Metrics(metrics)).await;
    assert_eq!(app.node_delays.get("n1"), Some(&42));
    assert!(
        app.metrics.node_delays.is_empty(),
        "the metrics snapshot must not keep a second delay map"
    );
    assert_eq!(app.view().node_delays.get("n1"), Some(&42));
}

#[tokio::test]
async fn new_connection_ids_only_flag_new_traffic() {
    let (mut app, _dir) = test_app().await;
    let connection = |id: &str| crate::singbox::clash::ConnectionInfo {
        id: id.to_string(),
        ..Default::default()
    };
    app.handle_event(AppEvent::Metrics(MetricsSnapshot {
        connections: vec![connection("a")],
        ..Default::default()
    }))
    .await;
    assert!(app.health.fast_probe_pending);

    app.health.fast_probe_pending = false;
    app.handle_event(AppEvent::Metrics(MetricsSnapshot {
        connections: vec![connection("a")],
        ..Default::default()
    }))
    .await;
    assert!(!app.health.fast_probe_pending);

    app.handle_event(AppEvent::Metrics(MetricsSnapshot {
        connections: vec![connection("a"), connection("b")],
        ..Default::default()
    }))
    .await;
    assert!(app.health.fast_probe_pending);
}

#[tokio::test]
async fn connection_ids_beyond_the_cap_do_not_rearm_fast_probes() {
    let (mut app, _dir) = test_app().await;
    let connections: Vec<_> = (0..=MAX_HEALTH_CONNECTION_IDS)
        .map(|id| crate::singbox::clash::ConnectionInfo {
            id: format!("c{id}"),
            ..Default::default()
        })
        .collect();
    let metrics = MetricsSnapshot {
        connections: connections.clone(),
        ..Default::default()
    };

    app.handle_event(AppEvent::Metrics(metrics.clone())).await;
    assert!(app.health.fast_probe_pending);

    app.health.fast_probe_pending = false;
    app.handle_event(AppEvent::Metrics(metrics)).await;
    assert!(
        !app.health.fast_probe_pending,
        "connections beyond the id cap must not look new on every poll"
    );
}

#[tokio::test]
async fn nodes_view_payload_stays_bounded_at_the_node_cap() {
    let (mut app, _dir) = test_app().await;
    let base = node("node");
    app.nodes = (0..crate::subscription::MAX_TOTAL_NODES)
        .map(|index| {
            let mut item = base.clone();
            item.tag = format!("n-{index:016x}");
            item.name = format!("node {index}");
            item
        })
        .collect();
    app.ui_state.page = Page::Nodes;
    for item in &app.nodes {
        app.node_delays.insert(item.tag.clone(), 42);
    }

    let view = app.view();
    assert_eq!(view.nodes.len(), crate::subscription::MAX_TOTAL_NODES);
    assert_eq!(view.auto_tags.len(), crate::subscription::MAX_TOTAL_NODES);
    let encoded = serde_json::to_vec(&view).unwrap();
    assert!(
        encoded.len() < 1_500_000,
        "Nodes View payload grew to {} bytes; consider windowing the node list",
        encoded.len()
    );
}

#[tokio::test]
async fn selection_storm_warns_once_per_window() {
    let (mut app, _dir) = test_app().await;
    let now = Instant::now();
    for _ in 0..SELECTION_STORM_THRESHOLD {
        app.record_selection_change(now);
    }
    let warnings = |app: &App| {
        app.logger
            .recent(20)
            .iter()
            .filter(|line| line.message.contains("selection storm"))
            .count()
    };
    assert_eq!(warnings(&app), 1);
    app.record_selection_change(now + Duration::from_secs(1));
    assert_eq!(
        warnings(&app),
        1,
        "cooldown must suppress repeated warnings"
    );
    assert_eq!(
        app.stats.selection_changes_total,
        SELECTION_STORM_THRESHOLD as u64 + 1
    );
}

#[tokio::test]
async fn observe_selection_change_counts_only_transitions() {
    let (mut app, _dir) = test_app().await;
    app.health.last_logged_selection = Some((true, Some("n-old".to_string())));
    app.metrics.selection.selected = Some(crate::singbox::tags::AUTO.to_string());
    app.metrics.selection.auto_now = Some("n-new".to_string());

    app.observe_selection_change(Instant::now());
    assert_eq!(app.stats.selection_changes_total, 1);
    assert_eq!(app.stats.selection_change_times.len(), 1);

    app.observe_selection_change(Instant::now());
    assert_eq!(app.stats.selection_changes_total, 1);
}

fn tagged_node(name: &str, tag: &str) -> Node {
    let mut item = node(name);
    item.tag = tag.to_string();
    item
}

fn health_outcome_for(status: u16) -> HealthOutcome {
    health_outcome(&Err(ClashError::Http(status)))
}

#[test]
fn multi_target_probe_only_fails_when_every_target_fails() {
    assert_eq!(probe_failure_outcome(true, false), HealthOutcome::Failed);
    assert_eq!(
        probe_failure_outcome(true, true),
        HealthOutcome::Unavailable
    );
    assert_eq!(
        probe_failure_outcome(false, true),
        HealthOutcome::Unavailable
    );
    assert_eq!(
        probe_failure_outcome(false, false),
        HealthOutcome::Unavailable
    );
}

#[test]
fn health_probe_outcomes_distinguish_node_and_api_errors() {
    assert_eq!(health_outcome(&Ok(120)), HealthOutcome::Healthy(120));
    assert_eq!(health_outcome(&Ok(0)), HealthOutcome::Failed);
    for status in [408, 500, 502, 503, 504] {
        assert_eq!(health_outcome_for(status), HealthOutcome::Failed);
    }
    for status in [400, 401, 404, 429] {
        assert_eq!(health_outcome_for(status), HealthOutcome::Unavailable);
    }
}

#[test]
fn failover_keeps_auto_mode_and_picks_the_best_quality_manual_node() {
    let healthy = vec![
        ("n-fast".to_string(), 40),
        ("n-slow".to_string(), 120),
        ("n-dead".to_string(), 300),
    ];
    let quality = QualityBook::default();
    assert_eq!(
        choose_recovery_target(Some("auto"), &healthy, &quality),
        Some(RecoveryTarget::Auto)
    );
    assert_eq!(
        choose_recovery_target(Some("n-manual"), &healthy, &quality),
        Some(RecoveryTarget::Node("n-fast".to_string()))
    );
    assert_eq!(choose_recovery_target(Some("auto"), &[], &quality), None);
}

#[tokio::test]
async fn health_probe_failures_are_counted_per_selected_node() {
    let (mut app, _dir) = test_app().await;
    app.proxy_runtime.status = ProxyStatus::Running;
    app.nodes = vec![tagged_node("A", "n1"), tagged_node("B", "n2")];
    app.proxy_runtime.proxy_node_tags = vec!["n1".to_string(), "n2".to_string()];
    app.metrics.selection.selected = Some("n1".to_string());

    let generation = app.health.generation;
    for _ in 0..HEALTH_FAILURE_THRESHOLD {
        app.handle_event(AppEvent::HealthProbe {
            generation,
            tag: "n1".to_string(),
            outcome: HealthOutcome::Failed,
        })
        .await;
    }
    assert_eq!(app.health.failures, HEALTH_FAILURE_THRESHOLD);
    assert_eq!(app.health.failure_tag.as_deref(), Some("n1"));

    app.handle_event(AppEvent::HealthProbe {
        generation,
        tag: "n1".to_string(),
        outcome: HealthOutcome::Healthy(35),
    })
    .await;
    assert_eq!(app.health.failures, 0);
    assert!(app.health.failure_tag.is_none());
    assert_eq!(app.node_delays.get("n1"), Some(&35));

    app.handle_event(AppEvent::HealthProbe {
        generation,
        tag: "n1".to_string(),
        outcome: HealthOutcome::Failed,
    })
    .await;
    assert_eq!(app.health.failures, 1);
    app.metrics.selection.selected = Some("n2".to_string());
    app.handle_event(AppEvent::HealthProbe {
        generation,
        tag: "n1".to_string(),
        outcome: HealthOutcome::Failed,
    })
    .await;
    assert_eq!(app.health.failures, 0);
}

#[tokio::test]
async fn only_new_proxy_connections_keep_health_probing_active() {
    let (mut app, _dir) = test_app().await;
    let mut metrics = MetricsSnapshot::default();
    metrics
        .connections
        .push(crate::singbox::clash::ConnectionInfo {
            id: "c1".to_string(),
            ..Default::default()
        });
    app.handle_event(AppEvent::Metrics(metrics.clone())).await;
    let first = app.health.last_activity.expect("first connection");

    app.health.last_activity = Some(Instant::now() - Duration::from_secs(10));
    let stale = app.health.last_activity.unwrap();
    app.handle_event(AppEvent::Metrics(metrics)).await;
    assert_eq!(app.health.last_activity, Some(stale));

    let mut next = MetricsSnapshot::default();
    next.connections
        .push(crate::singbox::clash::ConnectionInfo {
            id: "c2".to_string(),
            ..Default::default()
        });
    app.handle_event(AppEvent::Metrics(next)).await;
    assert!(app.health.last_activity.expect("new connection") > stale);
    assert!(app.health.last_activity.expect("new connection") >= first);
}

#[tokio::test]
async fn stale_health_probe_events_are_ignored() {
    let (mut app, _dir) = test_app().await;
    app.proxy_runtime.status = ProxyStatus::Running;
    app.nodes = vec![tagged_node("A", "n1")];
    app.proxy_runtime.proxy_node_tags = vec!["n1".to_string()];
    app.metrics.selection.selected = Some("n1".to_string());
    let generation = app.health.generation;
    app.invalidate_health_tasks();

    app.handle_event(AppEvent::HealthProbe {
        generation,
        tag: "n1".to_string(),
        outcome: HealthOutcome::Failed,
    })
    .await;
    assert_eq!(app.health.failures, 0);
}

#[tokio::test]
async fn recovery_candidates_respect_auto_filter_and_latency_order() {
    let (mut app, _dir) = test_app().await;
    app.proxy_runtime.status = ProxyStatus::Running;
    app.nodes = vec![
        tagged_node("香港 A", "n1"),
        tagged_node("日本 B", "n2"),
        tagged_node("香港 C", "n3"),
    ];
    app.proxy_runtime.proxy_node_tags = vec!["n1".to_string(), "n2".to_string(), "n3".to_string()];
    app.config.keywords_include = vec!["香港".to_string()];
    app.node_delays.insert("n2".to_string(), 10);
    app.node_delays.insert("n3".to_string(), 20);

    app.metrics.selection.selected = Some("auto".to_string());
    assert_eq!(app.recovery_candidates("n1"), vec!["n3".to_string()]);
    assert_eq!(app.recovery_candidates("n3"), vec!["n1".to_string()]);

    app.metrics.selection.selected = Some("n1".to_string());
    assert_eq!(
        app.recovery_candidates("n1"),
        vec!["n2".to_string(), "n3".to_string()]
    );
}

#[tokio::test]
async fn no_healthy_nodes_reports_the_failure_without_enabled_subscriptions() {
    let (mut app, _dir) = test_app().await;
    app.proxy_runtime.status = ProxyStatus::Running;
    app.on_no_healthy_nodes();
    assert_eq!(
        app.ui_state.last_error.as_deref(),
        Some(app.text().msg_health_no_nodes)
    );
    assert!(app.ui_state.message.is_some());
    assert!(!app.health.recovery_refresh_pending);
    assert!(app.health.pause_until.is_some_and(|at| at > Instant::now()));
    app.invalidate_health_tasks();
    assert!(app.health.pause_until.is_none());
}

#[tokio::test]
async fn no_node_pause_backs_off_progressively() {
    let (mut app, _dir) = test_app().await;
    app.proxy_runtime.status = ProxyStatus::Running;
    for expected in HEALTH_NO_NODE_BACKOFF_SECS {
        app.on_no_healthy_nodes();
        let remaining = app
            .health
            .pause_until
            .expect("pause")
            .saturating_duration_since(Instant::now())
            .as_secs();
        assert!(
            remaining <= expected && remaining + 1 >= expected,
            "{remaining} vs {expected}"
        );
    }
    app.on_no_healthy_nodes();
    let remaining = app
        .health
        .pause_until
        .expect("pause")
        .saturating_duration_since(Instant::now())
        .as_secs();
    let capped = *HEALTH_NO_NODE_BACKOFF_SECS.last().unwrap();
    assert!(remaining <= capped && remaining + 1 >= capped);
}

#[tokio::test]
async fn healthy_probe_clears_no_node_error_and_backoff() {
    let (mut app, _dir) = test_app().await;
    app.proxy_runtime.status = ProxyStatus::Running;
    app.nodes = vec![tagged_node("A", "n1")];
    app.proxy_runtime.proxy_node_tags = vec!["n1".to_string()];
    app.metrics.selection.selected = Some("n1".to_string());
    app.health.no_node_level = 2;
    app.health.pause_until = Some(Instant::now() - Duration::from_secs(1));
    let no_nodes = app.text().msg_health_no_nodes.to_string();
    app.ui_state.last_error = Some(no_nodes);
    let generation = app.health.generation;

    app.handle_event(AppEvent::HealthProbe {
        generation,
        tag: "n1".to_string(),
        outcome: HealthOutcome::Healthy(42),
    })
    .await;

    assert!(app.ui_state.last_error.is_none());
    assert!(app.health.pause_until.is_none());
    assert_eq!(app.health.no_node_level, 0);
}

#[test]
fn proxy_ip_failure_warnings_are_throttled() {
    assert_eq!(proxy_ip_failure_level(1, false), LogLevel::Warn);
    assert_eq!(proxy_ip_failure_level(2, false), LogLevel::Debug);
    assert_eq!(proxy_ip_failure_level(1, true), LogLevel::Debug);
}

#[test]
fn cached_subscription_age_uses_oldest_enabled_entry() {
    let config = |id: &str, enabled: bool| SubscriptionConfig {
        id: id.to_string(),
        name: id.to_string(),
        url: "https://example.com/sub".to_string(),
        enabled,
    };
    let now = chrono::Local::now();
    let stamp = |minutes: i64| {
        (now - chrono::Duration::minutes(minutes))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    };
    let mut fresh = empty_runtime(config("a", true));
    fresh.last_updated = Some(stamp(10));
    let mut old = empty_runtime(config("b", true));
    old.last_updated = Some(stamp(120));
    let disabled = empty_runtime(config("c", false));

    assert!(cached_subscription_age(&[fresh.clone()]).unwrap() < SUBSCRIPTION_STARTUP_MAX_AGE);
    assert!(cached_subscription_age(&[fresh.clone(), old]).unwrap() > SUBSCRIPTION_STARTUP_MAX_AGE);
    assert!(
        cached_subscription_age(&[fresh.clone(), disabled]).unwrap() < SUBSCRIPTION_STARTUP_MAX_AGE
    );
    assert!(cached_subscription_age(&[empty_runtime(config("d", true))]).is_none());
    assert!(cached_subscription_age(&[]).is_none());
}

async fn app_with_cached_subscription(last_updated: Option<String>) -> (App, TempDir) {
    let dir = TempDir::new().unwrap();
    let paths = Paths::discover(
        Some(dir.path().join("config.toml")),
        Some(dir.path().join("data")),
    )
    .unwrap();
    paths.ensure().unwrap();
    let mut config = Config::default();
    let subscription = SubscriptionConfig {
        id: "s-1".to_string(),
        name: "test".to_string(),
        url: "https://example.com/sub".to_string(),
        enabled: true,
    };
    config.subscriptions.push(subscription.clone());
    let mut runtime = empty_runtime(subscription);
    runtime.nodes = vec![node("cached")];
    runtime.last_updated = last_updated;
    subscription::cache::save(&paths.subscription_cache(), &[runtime]).unwrap();
    let logger = Logger::new(&paths.logs_dir, LogLevel::Info, LogLevel::Info).unwrap();
    (App::new(config, paths, logger).await, dir)
}

#[tokio::test]
async fn fresh_subscription_cache_skips_startup_refresh() {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let (app, _dir) = app_with_cached_subscription(Some(now)).await;
    assert!(!app.subscription_state.refreshing_subscriptions);
    assert!(app.subscription_state.last_subscription_refresh.is_some());

    let old = (chrono::Local::now() - chrono::Duration::hours(2))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let (app, _dir) = app_with_cached_subscription(Some(old)).await;
    assert!(app.subscription_state.refreshing_subscriptions);
}

#[tokio::test]
async fn failed_subscription_refresh_schedules_retry() {
    let (mut app, _dir) = test_app().await;
    let config = SubscriptionConfig {
        id: "s-1".to_string(),
        name: "test".to_string(),
        url: "https://example.com/sub".to_string(),
        enabled: true,
    };
    app.config.subscriptions.push(config.clone());
    let previous = Instant::now() - Duration::from_secs(25 * 3600);
    app.subscription_state.last_subscription_refresh = Some(previous);
    let seq = app.next_subscription_seq();
    app.subscription_state
        .subscription_refresh_seq
        .insert(config.id.clone(), seq);
    let mut runtime = empty_runtime(config);
    runtime.error = Some("timeout".to_string());

    app.handle_event(AppEvent::Subscriptions {
        seq,
        runtimes: vec![runtime],
    })
    .await;

    assert_eq!(
        app.subscription_state.last_subscription_refresh,
        Some(previous)
    );
    assert!(app
        .subscription_state
        .subscription_refresh_retry_at
        .is_some());
    assert!(!app.subscription_refresh_due());

    let seq = app.next_subscription_seq();
    app.subscription_state
        .subscription_refresh_seq
        .insert("s-1".to_string(), seq);
    let mut runtime = empty_runtime(app.config.subscriptions[0].clone());
    runtime.nodes = vec![node("fresh")];
    app.handle_event(AppEvent::Subscriptions {
        seq,
        runtimes: vec![runtime],
    })
    .await;
    assert!(app
        .subscription_state
        .subscription_refresh_retry_at
        .is_none());
    assert!(app
        .subscription_state
        .last_subscription_refresh
        .is_some_and(|last| last > previous));
}

#[tokio::test]
async fn view_scopes_heavy_lists_to_the_active_page() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("one"), node("two")];
    app.metrics
        .connections
        .push(crate::singbox::clash::ConnectionInfo {
            id: "c1".to_string(),
            host: "example.com".to_string(),
            destination_ip: String::new(),
            destination_port: "443".to_string(),
            network: "tcp".to_string(),
            upload: 1,
            download: 2,
            duration_secs: None,
        });

    app.ui_state.page = Page::Dashboard;
    let view = app.view();
    assert_eq!(view.node_count, 2);
    assert!(view.nodes.is_empty());
    assert_eq!(view.connection_count, 1);
    assert!(view.metrics.connections.is_empty());

    app.ui_state.page = Page::Nodes;
    let view = app.view();
    assert_eq!(view.nodes.len(), 2);

    app.ui_state.page = Page::Connections;
    let view = app.view();
    assert_eq!(view.metrics.connections.len(), 1);
}

#[tokio::test]
async fn auto_candidates_react_to_node_and_keyword_changes() {
    let (mut app, _dir) = test_app().await;
    app.nodes = vec![node("香港 01"), node("日本 02")];
    for (index, item) in app.nodes.iter_mut().enumerate() {
        item.tag = format!("n-{index}");
    }
    assert_eq!(app.auto_candidate_tags().len(), 2);

    app.config.keywords_include = vec!["日本".to_string()];
    assert_eq!(app.auto_candidate_tags(), vec!["n-1".to_string()]);

    app.config.keywords_include.clear();
    app.nodes.push(node("美国 03"));
    assert_eq!(app.auto_candidate_tags().len(), 3);
}
