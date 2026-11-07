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

pub struct Text {
    pub app_title: &'static str,

    pub status_running: &'static str,
    pub status_stopped: &'static str,
    pub status_starting: &'static str,
    pub status_stopping: &'static str,
    pub status_failed: &'static str,
    pub slogan: &'static str,

    pub dashboard_title: &'static str,
    pub listen: &'static str,
    pub selection_mode: &'static str,
    pub selection_mode_auto: &'static str,
    pub selection_mode_manual: &'static str,
    pub current_node: &'static str,
    pub node_removed_suffix: &'static str,
    pub node_unknown: &'static str,
    pub latency: &'static str,
    pub network: &'static str,
    pub local_ip: &'static str,
    pub proxy_ip: &'static str,
    pub connections: &'static str,
    pub binary_version: &'static str,
    pub subscription_summary: &'static str,
    pub last_error: &'static str,
    pub no_nodes: &'static str,
    pub no_matching_nodes: &'static str,
    pub no_error: &'static str,

    pub nodes_title: &'static str,
    pub nodes_empty: &'static str,
    pub nodes_name: &'static str,
    pub nodes_source: &'static str,
    pub nodes_protocol: &'static str,
    pub nodes_latency: &'static str,
    pub nodes_auto: &'static str,
    pub nodes_auto_candidates: &'static str,

    pub subscriptions_title: &'static str,
    pub subscriptions_empty: &'static str,
    pub subscriptions_name: &'static str,
    pub subscriptions_url: &'static str,
    pub subscriptions_updated: &'static str,
    pub subscriptions_nodes: &'static str,
    pub subscriptions_stale: &'static str,
    pub subscription_form_add_title: &'static str,
    pub subscription_form_edit_title: &'static str,
    pub subscription_form_name: &'static str,
    pub subscription_form_url: &'static str,
    pub subscription_form_enabled: &'static str,
    pub edit_form_hint: &'static str,
    pub subscription_form_invalid_url: &'static str,
    pub subscription_http_warning: &'static str,

    pub connections_title: &'static str,
    pub connections_empty: &'static str,
    pub connections_dest: &'static str,
    pub connections_network: &'static str,
    pub connections_traffic: &'static str,
    pub connections_duration: &'static str,

    pub logs_title: &'static str,
    pub logs_empty: &'static str,
    pub logs_empty_source: &'static str,
    pub logs_level: &'static str,
    pub logs_source: &'static str,
    pub logs_follow: &'static str,
    pub logs_detail_title: &'static str,
    pub logs_detail_hint: &'static str,

    pub settings_title: &'static str,
    pub settings_language: &'static str,
    pub settings_port: &'static str,
    pub settings_allow_lan: &'static str,
    pub settings_lan_auth: &'static str,
    pub settings_lan_auth_title: &'static str,
    pub settings_lan_username: &'static str,
    pub settings_lan_password: &'static str,
    pub settings_set: &'static str,
    pub settings_unset: &'static str,
    pub settings_lan_warning: &'static str,
    pub settings_lan_auth_warning: &'static str,
    pub settings_lan_warning_short: &'static str,
    pub settings_lan_auth_short: &'static str,
    pub settings_dns_servers: &'static str,
    pub settings_test_url: &'static str,
    pub settings_keywords_include: &'static str,
    pub settings_keywords_exclude: &'static str,
    pub settings_keywords_all: &'static str,
    pub settings_keywords_none: &'static str,
    pub settings_keywords_title: &'static str,
    pub settings_keywords_hint: &'static str,
    pub settings_keywords_saved: &'static str,
    pub settings_keywords_no_match: &'static str,
    pub settings_app_log_level: &'static str,
    pub settings_app_update: &'static str,
    pub app_update_off: &'static str,
    pub app_update_notify: &'static str,
    pub app_update_auto: &'static str,
    pub settings_singbox_log_level: &'static str,
    pub settings_binary_info: &'static str,
    pub settings_binary_installed: &'static str,
    pub settings_binary_upgrade: &'static str,
    pub settings_binary: &'static str,
    pub settings_config_path: &'static str,
    pub settings_log_file: &'static str,
    pub settings_read_only: &'static str,
    pub settings_copied: &'static str,
    pub settings_invalid_username: &'static str,
    pub settings_saved: &'static str,

    pub help_title: &'static str,
    pub version: &'static str,
    pub help_footer: &'static [FooterHint],
    pub help_sections: &'static [HelpSection],
    pub settings_key_hints: &'static str,
    pub setup_key_hints: &'static str,

    pub setup_title: &'static str,
    pub setup_current_version: &'static str,
    pub setup_latest_version: &'static str,
    pub setup_previous_version: &'static str,
    pub setup_status: &'static str,
    pub setup_not_installed: &'static str,
    pub setup_downloading: &'static str,
    pub setup_installing: &'static str,
    pub setup_switching: &'static str,
    pub setup_install_hint: &'static str,
    pub setup_busy: &'static str,
    pub setup_switch_failed: &'static str,
    pub setup_official_url: &'static str,
    pub setup_inbox_path: &'static str,
    pub setup_formats: &'static str,
    pub setup_checksum: &'static str,
    pub setup_verified: &'static str,
    pub setup_verified_ok: &'static str,
    pub setup_unverified: &'static str,
    pub setup_no_candidates: &'static str,
    pub setup_failed: &'static str,
    pub setup_confirm_unverified: &'static str,

    pub msg_proxy_not_running: &'static str,
    pub msg_proxy_busy: &'static str,
    pub msg_node_selected_pending: &'static str,
    pub msg_auto_pending: &'static str,
    pub msg_node_not_in_runtime: &'static str,
    pub msg_node_switching: &'static str,
    pub msg_preferred_missing: &'static str,
    pub msg_no_enabled_subscriptions: &'static str,
    pub msg_subscriptions_refreshed: &'static str,
    pub msg_node_selected: &'static str,
    pub msg_node_delay: &'static str,
    pub msg_auto_enabled: &'static str,
    pub msg_auto_restarting: &'static str,
    pub msg_health_recovering: &'static str,
    pub msg_health_switched: &'static str,
    pub msg_health_auto: &'static str,
    pub msg_health_no_nodes: &'static str,
    pub msg_health_refreshing: &'static str,
    pub msg_health_restarting: &'static str,
    pub msg_health_new_nodes: &'static str,
    pub msg_health_no_new_nodes: &'static str,
    pub msg_singbox_started: &'static str,
    pub msg_singbox_exited: &'static str,
    pub msg_ipv6_fallback: &'static str,
    pub msg_binary_installed: &'static str,
    pub msg_binary_switched: &'static str,
    pub msg_app_update_ready: &'static str,
    pub msg_app_update_applying: &'static str,
    pub msg_singbox_missing: &'static str,
    pub msg_rollback_confirm: &'static str,
    pub msg_rollback_unavailable: &'static str,
    pub msg_clash_api: &'static str,
    pub msg_clash_api_unknown: &'static str,
    pub msg_subscription_saved: &'static str,
    pub msg_subscription_deleted: &'static str,
    pub msg_subscription_delete_confirm: &'static str,
    pub msg_restart_scheduled: &'static str,
    pub msg_restart_paused: &'static str,
    pub msg_restart_confirm: &'static str,
    pub msg_restarting: &'static str,
    pub msg_preflight_failed: &'static str,
    pub msg_connecting: &'static str,
    pub msg_attach_busy: &'static str,
    pub msg_detached: &'static str,
    pub msg_daemon_closed: &'static str,
    pub msg_daemon_unavailable: &'static str,
    pub msg_daemon_incompatible: &'static str,

    pub common_on: &'static str,
    pub common_off: &'static str,
    pub common_auto: &'static str,
    pub common_none: &'static str,
    pub terminal_too_small: &'static str,
}

static ZH: Text = Text {
    app_title: "DFRelay",

    status_running: "运行中",
    status_stopped: "已停止",
    status_starting: "启动中",
    status_stopping: "停止中",
    status_failed: "失败",
    slogan: "东风一箭，万里一线",

    dashboard_title: "总览",
    listen: "监听",
    selection_mode: "节点方式",
    selection_mode_auto: "自动选择",
    selection_mode_manual: "手动指定",
    current_node: "当前节点",
    node_removed_suffix: "（已移除）",
    node_unknown: "未知节点",
    latency: "延迟",
    network: "网速",
    local_ip: "本机 IP",
    proxy_ip: "代理 IP",
    connections: "连接数",
    binary_version: "sing-box",
    subscription_summary: "订阅",
    last_error: "最近错误",
    no_nodes: "暂无节点，请先添加订阅",
    no_matching_nodes: "没有节点匹配关键词",
    no_error: "无",

    nodes_title: "节点",
    nodes_empty: "暂无节点，先添加订阅",
    nodes_name: "名称",
    nodes_source: "来源",
    nodes_protocol: "协议",
    nodes_latency: "延迟",
    nodes_auto: "自动",
    nodes_auto_candidates: "自动候选",

    subscriptions_title: "订阅",
    subscriptions_empty: "暂无订阅，按 n 添加",
    subscriptions_name: "名称",
    subscriptions_url: "URL",
    subscriptions_updated: "最后更新",
    subscriptions_nodes: "节点数",
    subscriptions_stale: "缓存",
    subscription_form_add_title: "新增订阅",
    subscription_form_edit_title: "编辑订阅",
    subscription_form_name: "名称",
    subscription_form_url: "URL",
    subscription_form_enabled: "启用",
    edit_form_hint: "Tab/↑↓ 切字段   ←/→ 光标   Enter 保存   Esc 取消",
    subscription_form_invalid_url: "URL 需以 http:// 或 https:// 开头",
    subscription_http_warning: "HTTP 订阅不加密，token 可能被窃听",

    connections_title: "连接",
    connections_empty: "暂无活动连接。",
    connections_dest: "目标",
    connections_network: "网络",
    connections_traffic: "流量",
    connections_duration: "时长",

    logs_title: "日志",
    logs_empty: "暂无日志。",
    logs_empty_source: "该来源暂无日志。",
    logs_level: "级别",
    logs_source: "来源",
    logs_follow: "跟随",
    logs_detail_title: "日志详情",
    logs_detail_hint: "j/k 滚动   g/G 首尾   Esc/Enter 关闭",

    settings_title: "设置",
    settings_language: "语言",
    settings_port: "端口",
    settings_allow_lan: "局域网访问",
    settings_lan_auth: "局域网认证",
    settings_lan_auth_title: "局域网认证",
    settings_lan_username: "用户名",
    settings_lan_password: "密码",
    settings_set: "已设置",
    settings_unset: "未设置",
    settings_lan_warning: "警告：0.0.0.0 无认证，仅限可信网络",
    settings_lan_auth_warning: "局域网认证已启用，仅限可信网络",
    settings_lan_warning_short: "LAN 无认证",
    settings_lan_auth_short: "LAN 认证已启用",
    settings_dns_servers: "DNS 服务器",
    settings_test_url: "测试 URL",
    settings_keywords_include: "包含关键词",
    settings_keywords_exclude: "排除关键词",
    settings_keywords_all: "全部节点",
    settings_keywords_none: "无",
    settings_keywords_title: "节点关键词",
    settings_keywords_hint: "逗号分隔   ←/→ 光标   Enter 保存   Esc 取消",
    settings_keywords_saved: "关键词已保存",
    settings_keywords_no_match: "没有匹配节点",
    settings_app_log_level: "应用日志",
    settings_app_update: "应用更新",
    app_update_off: "关",
    app_update_notify: "通知",
    app_update_auto: "自动",
    settings_singbox_log_level: "sing-box 日志",
    settings_binary_info: "sing-box 信息",
    settings_binary_installed: "已安装",
    settings_binary_upgrade: "可升级",
    settings_binary: "sing-box 路径",
    settings_config_path: "配置文件",
    settings_log_file: "日志文件",
    settings_read_only: "[RO]",
    settings_copied: "已复制",
    settings_invalid_username: "用户名不能包含冒号",
    settings_saved: "已保存",

    help_title: "帮助",
    version: "版本",
    help_footer: &[
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
    ],
    help_sections: &[
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
    ],
    settings_key_hints: "j/k 选择   Enter 操作",
    setup_key_hints: "j/k 选择   Enter 安装   b 切换",

    setup_title: "sing-box 信息",
    setup_current_version: "当前版本",
    setup_latest_version: "最新版本",
    setup_previous_version: "上一版本",
    setup_status: "状态",
    setup_not_installed: "未安装",
    setup_downloading: "正在下载 sing-box…",
    setup_installing: "正在安装 sing-box…",
    setup_switching: "正在切换 sing-box…",
    setup_install_hint: "按 d 下载最新版，或从 inbox 安装",
    setup_busy: "有任务正在进行，请稍后",
    setup_switch_failed: "切换失败",
    setup_official_url: "官方地址",
    setup_inbox_path: "放入目录",
    setup_formats: "支持 .tar.gz / .tgz / .zip / 裸二进制",
    setup_checksum: "可选 .sha256 校验；无校验需确认",
    setup_verified: "校验",
    setup_verified_ok: "已校验",
    setup_unverified: "未校验",
    setup_no_candidates: "没有可安装文件",
    setup_failed: "下载失败",
    setup_confirm_unverified: "再按 Enter 安装未校验文件，Esc 取消",

    msg_proxy_not_running: "代理未运行",
    msg_proxy_busy: "代理切换中，请稍后",
    msg_node_selected_pending: "节点已选，启动后生效",
    msg_auto_pending: "自动选路已启用，启动后生效",
    msg_node_not_in_runtime: "节点不在运行配置中，请重启代理",
    msg_node_switching: "节点不在运行配置中，正在重启切换",
    msg_preferred_missing: "原节点已不存在，改用自动",
    msg_no_enabled_subscriptions: "没有启用的订阅",
    msg_subscriptions_refreshed: "订阅已刷新",
    msg_node_selected: "已切换节点",
    msg_node_delay: "节点延迟",
    msg_auto_enabled: "已恢复自动选路",
    msg_auto_restarting: "自动候选已变化，正在重启代理",
    msg_health_recovering: "当前节点持续失败，正在重新选路",
    msg_health_switched: "当前节点持续失败，已切换到",
    msg_health_auto: "当前节点持续失败，已触发自动选路",
    msg_health_no_nodes: "所有节点失败",
    msg_health_refreshing: "所有节点不可用，正在更新订阅",
    msg_health_restarting: "订阅已更新，正在重启代理",
    msg_health_new_nodes: "订阅已更新，新节点启动后生效",
    msg_health_no_new_nodes: "订阅已更新，无新自动候选",
    msg_singbox_started: "sing-box 已启动",
    msg_singbox_exited: "sing-box 已退出",
    msg_ipv6_fallback: "IPv6 不可用，仅用 IPv4",
    msg_binary_installed: "已安装 sing-box",
    msg_binary_switched: "已切换 sing-box",
    msg_app_update_ready: "应用更新已就绪",
    msg_app_update_applying: "正在应用更新，daemon 将重启",
    msg_singbox_missing: "未安装 sing-box，请先安装",
    msg_rollback_confirm: "再按 b 切换版本，Esc 取消",
    msg_rollback_unavailable: "没有可切换的上一版",
    msg_clash_api: "Clash API",
    msg_clash_api_unknown: "API 未知",
    msg_subscription_saved: "订阅已保存",
    msg_subscription_deleted: "订阅已删除",
    msg_subscription_delete_confirm: "再按 d 删除，Esc 取消",
    msg_restart_scheduled: "sing-box 已退出，计划重启",
    msg_restart_paused: "sing-box 连续 5 次启动失败，已暂停；按 s 重试",
    msg_restart_confirm: "再按 R 重启，Esc 取消",
    msg_restarting: "正在重启代理",
    msg_preflight_failed: "配置校验失败，已保留当前代理",
    msg_connecting: "正在连接后台服务…",
    msg_attach_busy: "已在另一终端打开",
    msg_detached: "界面已退出，后台仍在运行；停止用 dfr stop 或服务管理器",
    msg_daemon_closed: "后台服务连接已断开",
    msg_daemon_unavailable: "无法连接后台服务",
    msg_daemon_incompatible: "daemon 版本不兼容，请运行 dfr restart",

    common_on: "开",
    common_off: "关",
    common_auto: "自动",
    common_none: "无",
    terminal_too_small: "终端太小，至少 60×16（建议 80×24）",
};

static EN: Text = Text {
    app_title: "DFRelay",

    status_running: "Running",
    status_stopped: "Stopped",
    status_starting: "Starting",
    status_stopping: "Stopping",
    status_failed: "Failed",
    slogan: "Twelve thousand in flight, the world in sight",

    dashboard_title: "Dashboard",
    listen: "Listen",
    selection_mode: "Selection",
    selection_mode_auto: "Auto",
    selection_mode_manual: "Manual",
    current_node: "Current node",
    node_removed_suffix: " (removed)",
    node_unknown: "Unknown node",
    latency: "Latency",
    network: "Network",
    local_ip: "Local IP",
    proxy_ip: "Proxy IP",
    connections: "Connections",
    binary_version: "sing-box",
    subscription_summary: "Subscriptions",
    last_error: "Last error",
    no_nodes: "No nodes; add a subscription first.",
    no_matching_nodes: "No nodes match keywords.",
    no_error: "None",

    nodes_title: "Nodes",
    nodes_empty: "No nodes; add a subscription first.",
    nodes_name: "Name",
    nodes_source: "Source",
    nodes_protocol: "Protocol",
    nodes_latency: "Latency",
    nodes_auto: "Auto",
    nodes_auto_candidates: "Auto candidates",

    subscriptions_title: "Subscriptions",
    subscriptions_empty: "No subscriptions; press n.",
    subscriptions_name: "Name",
    subscriptions_url: "URL",
    subscriptions_updated: "Updated",
    subscriptions_nodes: "Nodes",
    subscriptions_stale: "cached",
    subscription_form_add_title: "Add subscription",
    subscription_form_edit_title: "Edit subscription",
    subscription_form_name: "Name",
    subscription_form_url: "URL",
    subscription_form_enabled: "Enabled",
    edit_form_hint: "Tab/↑↓ field   ←/→ cursor   Enter save   Esc cancel",
    subscription_form_invalid_url: "URL must start with http:// or https://",
    subscription_http_warning: "HTTP subscription is not encrypted; token may be intercepted",

    connections_title: "Connections",
    connections_empty: "No active connections.",
    connections_dest: "Destination",
    connections_network: "Network",
    connections_traffic: "Traffic",
    connections_duration: "Duration",

    logs_title: "Logs",
    logs_empty: "No logs.",
    logs_empty_source: "No logs from this source.",
    logs_level: "Level",
    logs_source: "Source",
    logs_follow: "Follow",
    logs_detail_title: "Log detail",
    logs_detail_hint: "j/k scroll   g/G top/bottom   Esc/Enter close",

    settings_title: "Settings",
    settings_language: "Language",
    settings_port: "Port",
    settings_allow_lan: "LAN access",
    settings_lan_auth: "LAN auth",
    settings_lan_auth_title: "LAN authentication",
    settings_lan_username: "Username",
    settings_lan_password: "Password",
    settings_set: "set",
    settings_unset: "not set",
    settings_lan_warning: "Warning: no-auth proxy on 0.0.0.0; trusted networks only",
    settings_lan_auth_warning: "LAN auth on; trusted networks only",
    settings_lan_warning_short: "LAN: no auth",
    settings_lan_auth_short: "LAN: auth on",
    settings_dns_servers: "DNS servers",
    settings_test_url: "Test URL",
    settings_keywords_include: "Include keywords",
    settings_keywords_exclude: "Exclude keywords",
    settings_keywords_all: "all nodes",
    settings_keywords_none: "none",
    settings_keywords_title: "Node keywords",
    settings_keywords_hint: "Comma-separated   ←/→ cursor   Enter save   Esc cancel",
    settings_keywords_saved: "keywords saved",
    settings_keywords_no_match: "No matching nodes",
    settings_app_log_level: "App log",
    settings_app_update: "App update",
    app_update_off: "off",
    app_update_notify: "notify",
    app_update_auto: "auto",
    settings_singbox_log_level: "sing-box log",
    settings_binary_info: "sing-box info",
    settings_binary_installed: "installed",
    settings_binary_upgrade: "upgrade available",
    settings_binary: "sing-box path",
    settings_config_path: "Config file",
    settings_log_file: "Log file",
    settings_read_only: "[RO]",
    settings_copied: "copied",
    settings_invalid_username: "username cannot contain ':'",
    settings_saved: "saved",

    help_title: "Help",
    version: "Version",
    help_footer: &[
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
    ],
    help_sections: &[
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
    ],
    settings_key_hints: "j/k select   Enter action",
    setup_key_hints: "j/k select   Enter install   b prev",

    setup_title: "sing-box info",
    setup_current_version: "Current version",
    setup_latest_version: "Latest version",
    setup_previous_version: "Previous version",
    setup_status: "Status",
    setup_not_installed: "not installed",
    setup_downloading: "Downloading sing-box…",
    setup_installing: "Installing sing-box…",
    setup_switching: "Switching sing-box…",
    setup_install_hint: "Press d to download latest, or install from inbox",
    setup_busy: "another task is running",
    setup_switch_failed: "switch failed",
    setup_official_url: "Official URL",
    setup_inbox_path: "Inbox folder",
    setup_formats: "Supports .tar.gz / .tgz / .zip / raw binary",
    setup_checksum: "Optional .sha256; unverified needs confirmation",
    setup_verified: "Verification",
    setup_verified_ok: "verified",
    setup_unverified: "unverified",
    setup_no_candidates: "No installable file",
    setup_failed: "Download failed",
    setup_confirm_unverified: "Enter again to install unverified, Esc cancels",

    msg_proxy_not_running: "proxy is not running",
    msg_proxy_busy: "proxy busy; try again",
    msg_node_selected_pending: "node selected; applies on start",
    msg_auto_pending: "auto on; applies on start",
    msg_node_not_in_runtime: "node not in running config; restart first",
    msg_node_switching: "node not in running config; restarting to switch",
    msg_preferred_missing: "selected node gone; using auto",
    msg_no_enabled_subscriptions: "no enabled subscriptions",
    msg_subscriptions_refreshed: "subscriptions refreshed",
    msg_node_selected: "node selected",
    msg_node_delay: "node latency",
    msg_auto_enabled: "auto selection enabled",
    msg_auto_restarting: "auto candidates changed; restarting proxy",
    msg_health_recovering: "node failing; reselecting",
    msg_health_switched: "node failing; switched to",
    msg_health_auto: "node failing; auto reselect triggered",
    msg_health_no_nodes: "all nodes failed",
    msg_health_refreshing: "all nodes down; refreshing subscriptions",
    msg_health_restarting: "subscriptions updated; restarting proxy",
    msg_health_new_nodes: "subscriptions updated; new nodes on start",
    msg_health_no_new_nodes: "subscriptions updated; no new candidates",
    msg_singbox_started: "sing-box started",
    msg_singbox_exited: "sing-box exited",
    msg_ipv6_fallback: "IPv6 unavailable; using IPv4 only",
    msg_binary_installed: "sing-box installed",
    msg_binary_switched: "switched sing-box",
    msg_app_update_ready: "App update ready",
    msg_app_update_applying: "Applying update; daemon will restart",
    msg_singbox_missing: "sing-box not installed; open the sing-box info popup",
    msg_rollback_confirm: "b again to switch version, Esc to cancel",
    msg_rollback_unavailable: "no previous version",
    msg_clash_api: "Clash API",
    msg_clash_api_unknown: "API unknown",
    msg_subscription_saved: "subscription saved",
    msg_subscription_deleted: "subscription deleted",
    msg_subscription_delete_confirm: "d again to delete, Esc to cancel",
    msg_restart_scheduled: "sing-box exited; restart scheduled",
    msg_restart_paused: "sing-box failed 5 starts; paused; press s to retry",
    msg_restart_confirm: "R again to restart, Esc to cancel",
    msg_restarting: "restarting proxy",
    msg_preflight_failed: "config check failed; current proxy kept",
    msg_connecting: "connecting to daemon…",
    msg_attach_busy: "already open in another terminal",
    msg_detached:
        "TUI closed; DFRelay keeps running. Use dfr stop or the service manager to stop it",
    msg_daemon_closed: "daemon connection lost",
    msg_daemon_unavailable: "cannot reach daemon",
    msg_daemon_incompatible: "incompatible daemon; run dfr restart",

    common_on: "on",
    common_off: "off",
    common_auto: "auto",
    common_none: "none",
    terminal_too_small: "terminal too small; min 60x16 (80x24 recommended)",
};

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
    fn edit_form_hint_mentions_navigation_and_save() {
        for text in [&ZH, &EN] {
            assert!(text.edit_form_hint.contains("←/→"));
            assert!(text.edit_form_hint.contains("Enter"));
            assert!(text.settings_keywords_hint.contains("←/→"));
            assert!(text.settings_keywords_hint.contains("Enter"));
        }
    }
}
