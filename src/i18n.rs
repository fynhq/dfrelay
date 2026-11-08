use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Lang {
    #[default]
    Zh,
    En,
}

impl Lang {
    pub fn toggle(self) -> Self {
        match self {
            Self::Zh => Self::En,
            Self::En => Self::Zh,
        }
    }

    pub fn text(self) -> &'static Text {
        match self {
            Self::Zh => &ZH,
            Self::En => &EN,
        }
    }

    pub fn running_with_uptime(self, running: &str, uptime: &str) -> String {
        match self {
            Self::Zh => format!("{running}（{uptime}）"),
            Self::En => format!("{running} ({uptime})"),
        }
    }
}

pub struct HelpSection {
    pub title: &'static str,
    pub lines: &'static [&'static str],
}

pub struct FooterHint {
    pub key: &'static str,
    pub label: &'static str,
}

#[deny(dead_code)]
pub struct Text {
    pub(crate) app_title: &'static str,

    pub(crate) status_running: &'static str,
    pub(crate) status_stopped: &'static str,
    pub(crate) status_starting: &'static str,
    pub(crate) status_stopping: &'static str,
    pub(crate) status_failed: &'static str,
    pub(crate) slogan: &'static str,

    pub(crate) dashboard_title: &'static str,
    pub(crate) listen: &'static str,
    pub(crate) selection_mode: &'static str,
    pub(crate) selection_mode_auto: &'static str,
    pub(crate) selection_mode_manual: &'static str,
    pub(crate) current_node: &'static str,
    pub(crate) node_removed_suffix: &'static str,
    pub(crate) node_unknown: &'static str,
    pub(crate) latency: &'static str,
    pub(crate) network: &'static str,
    pub(crate) local_ip: &'static str,
    pub(crate) proxy_ip: &'static str,
    pub(crate) connections: &'static str,
    pub(crate) binary_version: &'static str,
    pub(crate) subscription_summary: &'static str,
    pub(crate) last_error: &'static str,
    pub(crate) no_nodes: &'static str,
    pub(crate) no_matching_nodes: &'static str,
    pub(crate) no_error: &'static str,

    pub(crate) nodes_title: &'static str,
    pub(crate) nodes_empty: &'static str,
    pub(crate) nodes_name: &'static str,
    pub(crate) nodes_source: &'static str,
    pub(crate) nodes_protocol: &'static str,
    pub(crate) nodes_latency: &'static str,
    pub(crate) nodes_auto: &'static str,
    pub(crate) nodes_auto_candidates: &'static str,

    pub(crate) subscriptions_title: &'static str,
    pub(crate) subscriptions_empty: &'static str,
    pub(crate) subscriptions_name: &'static str,
    pub(crate) subscriptions_url: &'static str,
    pub(crate) subscriptions_updated: &'static str,
    pub(crate) subscriptions_nodes: &'static str,
    pub(crate) subscriptions_stale: &'static str,
    pub(crate) subscription_form_add_title: &'static str,
    pub(crate) subscription_form_edit_title: &'static str,
    pub(crate) subscription_form_name: &'static str,
    pub(crate) subscription_form_url: &'static str,
    pub(crate) subscription_form_enabled: &'static str,
    pub(crate) edit_form_hint: &'static str,
    pub(crate) subscription_form_invalid_url: &'static str,
    pub(crate) subscription_http_warning: &'static str,

    pub(crate) connections_title: &'static str,
    pub(crate) connections_empty: &'static str,
    pub(crate) connections_dest: &'static str,
    pub(crate) connections_network: &'static str,
    pub(crate) connections_traffic: &'static str,
    pub(crate) connections_duration: &'static str,

    pub(crate) logs_title: &'static str,
    pub(crate) logs_empty: &'static str,
    pub(crate) logs_empty_source: &'static str,
    pub(crate) logs_level: &'static str,
    pub(crate) logs_source: &'static str,
    pub(crate) logs_follow: &'static str,
    pub(crate) logs_detail_title: &'static str,
    pub(crate) logs_detail_hint: &'static str,

    pub(crate) settings_title: &'static str,
    pub(crate) settings_language: &'static str,
    pub(crate) settings_port: &'static str,
    pub(crate) settings_allow_lan: &'static str,
    pub(crate) settings_lan_auth: &'static str,
    pub(crate) settings_lan_auth_title: &'static str,
    pub(crate) settings_lan_username: &'static str,
    pub(crate) settings_lan_password: &'static str,
    pub(crate) settings_set: &'static str,
    pub(crate) settings_unset: &'static str,
    pub(crate) settings_lan_warning: &'static str,
    pub(crate) settings_lan_auth_warning: &'static str,
    pub(crate) settings_lan_warning_short: &'static str,
    pub(crate) settings_lan_auth_short: &'static str,
    pub(crate) settings_dns_servers: &'static str,
    pub(crate) settings_test_url: &'static str,
    pub(crate) settings_keywords_include: &'static str,
    pub(crate) settings_keywords_exclude: &'static str,
    pub(crate) settings_keywords_all: &'static str,
    pub(crate) settings_keywords_none: &'static str,
    pub(crate) settings_keywords_title: &'static str,
    pub(crate) settings_keywords_hint: &'static str,
    pub(crate) settings_keywords_saved: &'static str,
    pub(crate) settings_keywords_no_match: &'static str,
    pub(crate) settings_app_log_level: &'static str,
    pub(crate) settings_app_update: &'static str,
    pub(crate) app_update_off: &'static str,
    pub(crate) app_update_notify: &'static str,
    pub(crate) app_update_auto: &'static str,
    pub(crate) settings_singbox_log_level: &'static str,
    pub(crate) settings_binary_info: &'static str,
    pub(crate) settings_binary_installed: &'static str,
    pub(crate) settings_binary_upgrade: &'static str,
    pub(crate) settings_binary: &'static str,
    pub(crate) settings_config_path: &'static str,
    pub(crate) settings_log_file: &'static str,
    pub(crate) settings_read_only: &'static str,
    pub(crate) settings_copied: &'static str,
    pub(crate) settings_invalid_username: &'static str,
    pub(crate) settings_saved: &'static str,

    pub(crate) help_title: &'static str,
    pub(crate) version: &'static str,
    pub(crate) help_footer: &'static [FooterHint],
    pub(crate) help_sections: &'static [HelpSection],
    pub(crate) settings_key_hints: &'static str,
    pub(crate) setup_key_hints: &'static str,

    pub(crate) setup_title: &'static str,
    pub(crate) setup_current_version: &'static str,
    pub(crate) setup_latest_version: &'static str,
    pub(crate) setup_previous_version: &'static str,
    pub(crate) setup_status: &'static str,
    pub(crate) setup_not_installed: &'static str,
    pub(crate) setup_downloading: &'static str,
    pub(crate) setup_installing: &'static str,
    pub(crate) setup_switching: &'static str,
    pub(crate) setup_install_hint: &'static str,
    pub(crate) setup_busy: &'static str,
    pub(crate) setup_switch_failed: &'static str,
    pub(crate) setup_official_url: &'static str,
    pub(crate) setup_inbox_path: &'static str,
    pub(crate) setup_formats: &'static str,
    pub(crate) setup_checksum: &'static str,
    pub(crate) setup_verified: &'static str,
    pub(crate) setup_verified_ok: &'static str,
    pub(crate) setup_unverified: &'static str,
    pub(crate) setup_no_candidates: &'static str,
    pub(crate) setup_failed: &'static str,
    pub(crate) setup_confirm_unverified: &'static str,

    pub(crate) msg_proxy_not_running: &'static str,
    pub(crate) msg_proxy_busy: &'static str,
    pub(crate) msg_node_selected_pending: &'static str,
    pub(crate) msg_auto_pending: &'static str,
    pub(crate) msg_node_not_in_runtime: &'static str,
    pub(crate) msg_node_switching: &'static str,
    pub(crate) msg_preferred_missing: &'static str,
    pub(crate) msg_no_enabled_subscriptions: &'static str,
    pub(crate) msg_subscriptions_refreshed: &'static str,
    pub(crate) msg_node_selected: &'static str,
    pub(crate) msg_node_delay: &'static str,
    pub(crate) msg_auto_enabled: &'static str,
    pub(crate) msg_auto_restarting: &'static str,
    pub(crate) msg_health_recovering: &'static str,
    pub(crate) msg_health_switched: &'static str,
    pub(crate) msg_health_auto: &'static str,
    pub(crate) msg_health_no_nodes: &'static str,
    pub(crate) msg_health_refreshing: &'static str,
    pub(crate) msg_health_restarting: &'static str,
    pub(crate) msg_health_new_nodes: &'static str,
    pub(crate) msg_health_no_new_nodes: &'static str,
    pub(crate) msg_singbox_started: &'static str,
    pub(crate) msg_singbox_exited: &'static str,
    pub(crate) msg_ipv6_fallback: &'static str,
    pub(crate) msg_binary_installed: &'static str,
    pub(crate) msg_binary_switched: &'static str,
    pub(crate) msg_app_update_ready: &'static str,
    pub(crate) msg_app_update_applying: &'static str,
    pub(crate) msg_singbox_missing: &'static str,
    pub(crate) msg_rollback_confirm: &'static str,
    pub(crate) msg_rollback_unavailable: &'static str,
    pub(crate) msg_clash_api: &'static str,
    pub(crate) msg_clash_api_unknown: &'static str,
    pub(crate) msg_subscription_saved: &'static str,
    pub(crate) msg_subscription_deleted: &'static str,
    pub(crate) msg_subscription_delete_confirm: &'static str,
    pub(crate) msg_restart_scheduled: &'static str,
    pub(crate) msg_restart_paused: &'static str,
    pub(crate) msg_restart_confirm: &'static str,
    pub(crate) msg_restarting: &'static str,
    pub(crate) msg_preflight_failed: &'static str,
    pub(crate) msg_connecting: &'static str,
    pub(crate) msg_attach_busy: &'static str,
    pub(crate) msg_detached: &'static str,
    pub(crate) msg_daemon_closed: &'static str,
    pub(crate) msg_daemon_unavailable: &'static str,
    pub(crate) msg_daemon_incompatible: &'static str,

    pub(crate) common_on: &'static str,
    pub(crate) common_off: &'static str,
    pub(crate) common_auto: &'static str,
    pub(crate) common_none: &'static str,
    pub(crate) terminal_too_small: &'static str,
}

const ZH_HELP_FOOTER: &[FooterHint] = &[
    FooterHint {
        key: "L",
        label: "Language",
    },
    FooterHint {
        key: "?",
        label: "帮助",
    },
    FooterHint {
        key: "q",
        label: "退出界面",
    },
];
const ZH_HELP_SECTIONS: &[HelpSection] = &[
    HelpSection {
        title: "全局",
        lines: &[
            "q / Ctrl-C  退出界面，后台继续运行",
            "dfr stop  停止 ad-hoc daemon；服务用户用 systemctl/launchctl",
            "?  帮助；再按 ? 或 Esc 返回",
            "h / l 或 ←/→  切页",
            "s  启动/停止代理",
            "R  重启代理（两次确认）",
            "r  刷新当前页（含代理 IP）",
            "L  切换语言",
            "j/k 或 ↓↑  导航/滚动；g/G 或 Home/End 首尾",
        ],
    },
    HelpSection {
        title: "总览",
        lines: &["Enter  有错误时跳日志"],
    },
    HelpSection {
        title: "节点",
        lines: &[
            "Enter  选择节点（停止时启动后生效）",
            "a  自动选路",
            "n  名称/默认排序",
            "t  测试选中节点",
            "r  重测全部节点",
        ],
    },
    HelpSection {
        title: "订阅",
        lines: &["n  新增", "e / Enter  编辑", "d  两次删除"],
    },
    HelpSection {
        title: "连接",
        lines: &["j/k  浏览连接", "Enter  复制目标"],
    },
    HelpSection {
        title: "日志",
        lines: &[
            "j/k  滚动；到底自动跟随",
            "f  跟随开关",
            "d  切换时间显示（时间/日期时间/隐藏）",
            "v  循环级别",
            "x  切换来源（全部/app/sing-box）",
            "c  清空视图（不删文件）",
            "Enter  查看详情（j/k、g/G 滚动）",
        ],
    },
    HelpSection {
        title: "设置",
        lines: &[
            "j/k  选择",
            "Enter  切换/编辑/复制/打开",
            "sing-box 信息：状态；Enter 打开弹窗",
            "端口/DNS/测试 URL 需改 config.toml；路径只读",
            "局域网认证：用户名和密码同时设置才启用",
            "弹窗：Tab/↑↓ 切字段，←/→ 光标，▸ 当前字段",
            "Esc  关闭弹窗",
        ],
    },
    HelpSection {
        title: "设置/sing-box",
        lines: &[
            "d  下载/更新最新版",
            "b  切换上一版（两次确认）",
            "sing-box 日志只读；改 config.toml 后重启",
            "p  复制路径",
            "r  重扫 inbox",
            "Enter  安装选中文件",
            "Esc  返回",
        ],
    },
];
const EN_HELP_FOOTER: &[FooterHint] = &[
    FooterHint {
        key: "L",
        label: "语言",
    },
    FooterHint {
        key: "?",
        label: "help",
    },
    FooterHint {
        key: "q",
        label: "exit TUI",
    },
];
const EN_HELP_SECTIONS: &[HelpSection] = &[
    HelpSection {
        title: "Global",
        lines: &[
            "q / Ctrl-C  exit TUI; daemon keeps running",
            "dfr stop  stop ad-hoc daemon; service users use systemctl/launchctl",
            "?  help; ? or Esc returns",
            "h / l or ←/→  page",
            "s  start/stop proxy",
            "R  restart proxy (twice)",
            "r  refresh page (incl. proxy IP)",
            "L  language",
            "j/k or ↓↑  navigate/scroll; g/G or Home/End top/bottom",
        ],
    },
    HelpSection {
        title: "Dashboard",
        lines: &["Enter  jump to Logs on error"],
    },
    HelpSection {
        title: "Nodes",
        lines: &[
            "Enter  select node (applies on start)",
            "a  auto selection",
            "n  name/default sort",
            "t  test selected node",
            "r  retest all nodes",
        ],
    },
    HelpSection {
        title: "Subscriptions",
        lines: &["n  add", "e / Enter  edit", "d  twice to delete"],
    },
    HelpSection {
        title: "Connections",
        lines: &["j/k  browse connections", "Enter  copy target"],
    },
    HelpSection {
        title: "Logs",
        lines: &[
            "j/k  scroll; bottom resumes follow",
            "f  follow tail",
            "d  cycle time display (time/datetime/hidden)",
            "v  cycle level",
            "x  cycle source (all/app/sing-box)",
            "c  clear view (files kept)",
            "Enter  detail (j/k, g/G)",
        ],
    },
    HelpSection {
        title: "Settings",
        lines: &[
            "j/k  select",
            "Enter  toggle/edit/copy/open",
            "sing-box info: status; Enter opens the popup",
            "Port/DNS/Test URL need config.toml; paths read-only",
            "LAN auth: set both username and password",
            "Popup: Tab/↑↓ field, ←/→ cursor, ▸ active",
            "Esc  close popup",
        ],
    },
    HelpSection {
        title: "Settings/sing-box",
        lines: &[
            "d  download/update latest",
            "b  switch previous (twice)",
            "sing-box log is read-only; edit config.toml and restart",
            "p  copy path",
            "r  rescan inbox",
            "Enter  install selected",
            "Esc  back",
        ],
    },
];

// Generated by build.rs from locales/zh.conf and locales/en.conf.
include!(concat!(env!("OUT_DIR"), "/i18n_gen.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_chinese() {
        assert_eq!(Lang::default(), Lang::Zh);
        assert_eq!(Lang::default().text().dashboard_title, "总览");
    }

    #[test]
    fn toggle_has_both_languages() {
        assert_eq!(Lang::Zh.toggle(), Lang::En);
        assert_eq!(Lang::En.toggle(), Lang::Zh);
        assert_ne!(Lang::Zh.text().logs_title, Lang::En.text().logs_title);
    }

    #[test]
    fn help_documents_the_log_source_cycle() {
        for (lang, needle) in [(Lang::Zh, "切换来源"), (Lang::En, "cycle source")] {
            assert!(
                lang.text()
                    .help_sections
                    .iter()
                    .any(|section| section.lines.iter().any(|line| line.contains(needle))),
                "{needle}"
            );
        }
    }

    #[test]
    fn running_status_includes_uptime_with_language_punctuation() {
        assert_eq!(
            Lang::Zh.running_with_uptime("运行中", "01:02:03"),
            "运行中（01:02:03）"
        );
        assert_eq!(
            Lang::En.running_with_uptime("Running", "01:02:03"),
            "Running (01:02:03)"
        );
    }

    #[test]
    fn every_text_field_is_non_empty() {
        for text in [&ZH, &EN] {
            assert!(!text.app_title.is_empty());
            assert!(!text.setup_install_hint.is_empty());
            assert!(!text.setup_current_version.is_empty());
            assert!(!text.setup_latest_version.is_empty());
            assert!(!text.setup_previous_version.is_empty());
            assert!(!text.setup_status.is_empty());
            assert!(!text.setup_downloading.is_empty());
            assert!(!text.setup_installing.is_empty());
            assert!(!text.setup_switching.is_empty());
            assert!(!text.setup_busy.is_empty());
            assert!(!text.setup_switch_failed.is_empty());
            assert!(!text.settings_binary_info.is_empty());
            assert!(!text.settings_binary_installed.is_empty());
            assert!(!text.settings_binary_upgrade.is_empty());
            assert!(!text.msg_singbox_missing.is_empty());
            assert!(!text.msg_node_switching.is_empty());
            assert!(!text.msg_binary_switched.is_empty());
            assert!(!text.msg_rollback_confirm.is_empty());
            assert!(!text.msg_rollback_unavailable.is_empty());
        }
    }

    #[test]
    fn english_help_sections_have_no_chinese() {
        fn has_cjk(value: &str) -> bool {
            value
                .chars()
                .any(|c| matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'))
        }
        let text = Lang::En.text();
        assert!(text.help_sections.iter().all(|section| {
            !has_cjk(section.title) && section.lines.iter().all(|line| !has_cjk(line))
        }));
    }

    #[test]
    fn edit_form_hint_mentions_navigation_and_save() {
        for text in [&ZH, &EN] {
            assert!(text.edit_form_hint.contains("←/→"));
            assert!(text.edit_form_hint.contains("Enter"));
            assert!(text.settings_keywords_hint.contains("←/→"));
            assert!(text.settings_keywords_hint.contains("Enter"));
        }
    }
}
