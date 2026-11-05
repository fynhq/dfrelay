<a id="dfrelay-en"></a>

# DFRelay

DFRelay is a terminal UI proxy manager for Linux, macOS, and Android Termux
(arm64). It runs the official [sing-box](https://github.com/SagerNet/sing-box)
as an external process and exposes an HTTP/SOCKS mixed proxy on loopback by
default.

**DFRelay is not a VPN and provides no TUN, system proxy, GUI/web UI, or remote
control.**

English | [中文](#dfrelay-zh)

## Features

- Loopback HTTP/SOCKS mixed proxy (`127.0.0.1:12000`, plus `[::1]:12000` when
  available); optional LAN access with HTTP/SOCKS authentication.
- Clash/Mihomo YAML, Base64 URI lists, sing-box JSON, and plain URI
  subscriptions; latency-based selection, keyword filters, manual switching,
  and failed-node recovery.
- TUI with live traffic, connection counts, current node, local/proxy IP, logs
  with level and app/sing-box source filters, and a sing-box management popup.
- Headless daemon for systemd/launchd, with foreground `dfr serve`; the TUI is a
  client and never owns the daemon lifecycle.
- Low memory footprint: about 32 MB resident excluding the TUI (`dfr` 5–6 MB,
  sing-box 26–28 MB).
- Optional self-update with digest/SHA256SUMS verification, automatic rollback,
  and an `off`/`notify`/`auto` policy (default `notify`; system-level services
  apply manually); sing-box updates stay manual.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/fynhq/dfrelay/main/install.sh | sh
```

The installer downloads the latest release, verifies it against `SHA256SUMS`,
and installs or upgrades `dfr` in place. It never invokes `sudo`: if the target
directory is not writable, rerun it with `sudo`.

On Android Termux (arm64), install the required tools first:

```bash
pkg install curl tar coreutils
```

`dfr` is a short name and may collide with another tool; use `dfr --version` or
the full path when in doubt.

## Usage

```bash
dfr                 # attach the TUI; the daemon must already be running
dfr start           # start an ad-hoc daemon, or the configured service
dfr stop            # stop an ad-hoc daemon, or the configured service
dfr restart         # restart an ad-hoc daemon, or the configured service
dfr status          # show daemon mode, proxy state, node, and listen address
dfr serve           # run the daemon in the foreground for systemd/launchd
dfr service forget  # remove the service marker after removing the service
dfr update check    # check the latest DFRelay release
dfr update apply    # download, verify, install, and restart
dfr update rollback # restore the previous binary
dfr update status   # show the update policy and last result
```

`dfr` is a pure client. If the daemon is not running, start it with `dfr start`
or a service manager. `q` / `Ctrl-C` only exit the TUI; the daemon, proxy,
subscriptions, and logs keep running. For supervisor-managed deployments, see
[Service templates](#service-templates).

On first use, run `dfr start`, then `dfr`; install sing-box from its popup, add
a subscription with `n`, select nodes, and start the proxy with `s`. Point
applications at `http://127.0.0.1:12000` or `socks5://127.0.0.1:12000`.

Main keys: `q` exit, `s` start/stop proxy, `R` restart proxy (twice), `L`
language, `?` help, `h`/`l` or arrows switch pages, and `j`/`k` or arrows
move/scroll.

On Android/Termux, only programs inside Termux or apps with a manual HTTP/SOCKS
proxy can use DFRelay. Run `termux-wake-lock` while keeping the daemon in the
background; there is no VPN or Android system-wide proxy.

## Configuration and logs

Linux, macOS, and Termux use XDG paths:

```text
$XDG_CONFIG_HOME/dfrelay/config.toml   # fallback: ~/.config/dfrelay/config.toml
$XDG_DATA_HOME/dfrelay/                # fallback: ~/.local/share/dfrelay/
```

The data directory holds the managed sing-box binary, subscription cache,
runtime configuration, inbox, and daily logs (kept for 7 days). Most settings
are in the TUI; port, DNS, test URL, sing-box log level, and `bypass_lan` are
edited only in `config.toml`, and changes require a daemon restart
(`dfr restart`). `--config` (`-c`) and `--data` (`-d`) override the XDG paths.

The managed sing-box can be downloaded or updated from its popup; the official
release digest is the trust anchor, and the previous binary is kept for
rollback.

<a id="service-templates"></a>

## Service templates

Run `dfr start` + `dfr` once to install sing-box and configure subscriptions
before enabling a service. `dfr serve` starts the proxy automatically when the
configuration is valid. Choose one supervisor below; do not enable both. The
templates set `DFRELAY_SERVICE=1` and the service identity; keep the same
`--data` directory for the service and the CLI. With the identity, `dfr start`,
`dfr stop`, and `dfr restart` invoke the supervisor while the daemon is
stopped; `dfr start` on a running daemon only asks it to start the proxy.
Without the identity they refuse and print the supervisor command. System-level
services require root and are not controlled automatically; DFRelay never runs
`sudo`. Existing units that set only `DFRELAY_SERVICE=1` keep the refusal
behavior. A recorded service marker never falls back to ad-hoc: after removing
a service, delete `<data-dir>/service.json` or run `dfr service forget` (daemon
stopped) before using `dfr start` ad hoc.

### Linux systemd (user service)

Unit file: `~/.config/systemd/user/dfrelay.service`

```ini
[Unit]
Description=DFRelay proxy manager
After=network.target

[Service]
ExecStart=/usr/local/bin/dfr serve
Environment=DFRELAY_SERVICE=1
Environment=DFRELAY_SERVICE_MANAGER=systemd-user
Environment=DFRELAY_SERVICE_UNIT=dfrelay.service
Restart=on-failure
RestartSec=3
TimeoutStopSec=10
KillMode=mixed
LimitNOFILE=4096

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now dfrelay.service
loginctl enable-linger "$USER"   # optional: start without an interactive login
systemctl --user restart dfrelay.service
systemctl --user stop dfrelay.service
```

Adjust `ExecStart` if `dfr` is installed outside `/usr/local/bin`; keep
`DFRELAY_SERVICE_UNIT` equal to the unit file name. `dfr stop` stops the
service now but leaves it enabled; to keep it stopped across logins, run
`systemctl --user disable --now dfrelay.service`. A system-wide service can use
`User=` and an explicit `HOME`; a user service is recommended.

### macOS launchd (LaunchAgent)

File: `~/Library/LaunchAgents/io.github.fynhq.dfrelay.plist`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>io.github.fynhq.dfrelay</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/dfr</string>
    <string>serve</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>DFRELAY_SERVICE</key>
    <string>1</string>
    <key>DFRELAY_SERVICE_MANAGER</key>
    <string>launchd-gui</string>
    <key>DFRELAY_SERVICE_UNIT</key>
    <string>io.github.fynhq.dfrelay</string>
    <key>DFRELAY_SERVICE_TARGET</key>
    <string>/Users/YOUR_USER/Library/LaunchAgents/io.github.fynhq.dfrelay.plist</string>
    <key>HOME</key>
    <string>/Users/YOUR_USER</string>
    <key>PATH</key>
    <string>/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>ExitTimeOut</key>
  <integer>20</integer>
  <key>AbandonProcessGroup</key>
  <false/>
  <key>WorkingDirectory</key>
  <string>/Users/YOUR_USER</string>
</dict>
</plist>
```

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.fynhq.dfrelay.plist
launchctl kickstart -k gui/$(id -u)/io.github.fynhq.dfrelay
launchctl bootout gui/$(id -u)/io.github.fynhq.dfrelay
```

Replace `YOUR_USER` and adjust the binary path and target. Keep the label and
path in `DFRELAY_SERVICE_UNIT`/`DFRELAY_SERVICE_TARGET` equal to the plist
`Label` and path. A LaunchAgent starts at login. `dfr stop` uses
`launchctl bootout`; the plist remains and is loaded again at the next login,
and the target path is required to bootstrap the job again after a stop. A
system-wide LaunchDaemon in `/Library/LaunchDaemons` is not controlled by
`dfr`. The plist omits `StandardOutPath`/`StandardErrorPath` because
`dfr serve` writes daily logs to the data directory; redirected output stays
empty.

## Security

- The proxy binds loopback by default. LAN access is opt-in and binds IPv4
  `0.0.0.0`; when both a LAN username and password are set, the mixed proxy
  requires HTTP/SOCKS authentication. Authentication is access control, not
  traffic encryption.
- Private, link-local, ULA, and local-only targets bypass the proxy by default.
- Subscription URLs and node credentials are treated as secrets; configuration,
  cache, and logs use private permissions. DFRelay does not change system DNS or
  configure a system proxy.

## Third-party software

DFRelay does not bundle sing-box. The official binary is downloaded separately
under GPL-3.0-or-later and stored in the DFRelay data directory; custom external
sing-box paths are not supported.

## License

Released under the MIT license. See [LICENSE](LICENSE).

---

<a id="dfrelay-zh"></a>

# DFRelay

DFRelay 是一个面向 Linux、macOS 与 Android Termux（arm64）的终端 TUI
代理管理器。它以外部进程方式运行官方
[sing-box](https://github.com/SagerNet/sing-box)，默认在本机回环地址提供 HTTP/SOCKS
二合一代理。

**DFRelay 不是 VPN，不提供 TUN、系统代理、GUI/Web 界面或远程控制。**

[English](#dfrelay-en) | 中文

## 功能

- 回环 HTTP/SOCKS 二合一代理（`127.0.0.1:12000`，IPv6 可用时同时监听
  `[::1]:12000`）；可选局域网访问与 HTTP/SOCKS 认证。
- 订阅支持 Clash/Mihomo YAML、Base64 URI 列表、sing-box JSON 与纯 URI；
  支持延迟自动选路、关键词过滤、手动切换和失败节点恢复。
- TUI 展示网速、连接数、当前节点、本机/代理 IP，支持按级别与 app/sing-box
  来源过滤日志，并提供 sing-box 管理弹窗。
- 无界面 daemon 可由 systemd/launchd 托管；TUI 只是客户端，不负责 daemon 生命周期。
- 内存占用低：常驻约 32 MB（不含 TUI，`dfr` 5–6 MB、sing-box 26–28 MB）。
- 可选应用自更新：校验 digest/SHA256SUMS、失败自动回滚；支持
  `off`/`notify`/`auto`（默认 `notify`，系统级服务需手动应用）。sing-box 更新
  仍为手动。

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/fynhq/dfrelay/main/install.sh | sh
```

脚本下载最新发布包，用 `SHA256SUMS` 校验后原位安装或升级 `dfr`。脚本不会调用
`sudo`：目标目录不可写时，请用 `sudo` 重新执行。

Android Termux（arm64）先安装依赖：

```bash
pkg install curl tar coreutils
```

`dfr` 是短命令名，可能与其他工具冲突；用 `dfr --version` 或完整路径确认。

## 使用

```bash
dfr                 # 连接 TUI；daemon 必须已经运行
dfr start           # 后台启动 ad-hoc daemon，或启动已配置的系统服务
dfr stop            # 停止 ad-hoc daemon，或停止已配置的系统服务
dfr restart         # 重启 ad-hoc daemon，或重启已配置的系统服务
dfr status          # 查看模式、代理状态、当前节点和监听地址
dfr serve           # 前台运行 daemon，供 systemd/launchd 托管
dfr service forget  # 移除服务后清理 service marker
dfr update check    # 检查 DFRelay 最新版本
dfr update apply    # 下载、校验、安装并重启
dfr update rollback # 恢复上一版二进制
dfr update status   # 查看更新策略与最近结果
```

`dfr` 只是客户端。daemon 没运行时应执行 `dfr start` 或通过服务管理器启动。
`q` / `Ctrl-C` 只退出界面，daemon、代理、订阅和日志继续运行。服务托管部署见
[服务模板](#service-templates-zh)。

首次使用先执行 `dfr start`，再执行 `dfr`；在 sing-box 弹窗中安装 sing-box，按
`n` 添加订阅，选择节点后按 `s` 启动。应用代理指向 `http://127.0.0.1:12000` 或
`socks5://127.0.0.1:12000`。

主要键位：`q` 退出，`s` 启动/停止代理，`R` 重启代理（两次确认），`L` 切换语言，
`?` 帮助，`h`/`l` 或方向键切页，`j`/`k` 或方向键移动/滚动。

Android/Termux 只提供本地代理：只有 Termux 内程序或手动设置 HTTP/SOCKS 代理的
App 可用；没有 VPN 或 Android 全局代理；后台运行时建议执行 `termux-wake-lock`。

## 配置与日志

Linux、macOS 与 Termux 遵循 XDG：

```text
$XDG_CONFIG_HOME/dfrelay/config.toml   # 兜底：~/.config/dfrelay/config.toml
$XDG_DATA_HOME/dfrelay/                # 兜底：~/.local/share/dfrelay/
```

数据目录包含受管 sing-box、订阅缓存、运行时配置、inbox 和按天日志（保留 7 天）。
大部分设置可在 TUI 修改；端口、DNS、测试 URL、sing-box 日志级别和 `bypass_lan`
只能编辑 `config.toml`，修改后需重启 daemon（`dfr restart`）。`--config`（`-c`）、
`--data`（`-d`）可覆盖 XDG 路径。

受管 sing-box 可在其弹窗中下载/更新；始终以官方 release digest 校验，上一版保留
用于回滚。

<a id="service-templates-zh"></a>

## 服务模板

建议先用 `dfr start` + `dfr` 完成 sing-box 安装与订阅配置，再启用服务。配置正常时
`dfr serve` 会自动启动代理。下面二选一，不要同时启用。模板会设置
`DFRELAY_SERVICE=1` 与服务身份；服务与 CLI 应使用相同的 `--data` 目录。配置身份
后，daemon 停止时 `dfr start/stop/restart` 会调用服务管理器；daemon 运行中
`dfr start` 只请求启动代理。身份缺失时拒绝并打印手动命令。系统级服务需要 root，
DFRelay 不会自动控制，也不会自动执行 `sudo`。仅设置 `DFRELAY_SERVICE=1` 的旧
unit 仍保持拒绝行为。marker 存在时不会回退 ad-hoc：移除服务后，请先删除
`<data-dir>/service.json`，或在 daemon 停止后执行 `dfr service forget`，再使用
`dfr start`。

### Linux systemd（用户服务）

Unit 文件：`~/.config/systemd/user/dfrelay.service`

```ini
[Unit]
Description=DFRelay proxy manager
After=network.target

[Service]
ExecStart=/usr/local/bin/dfr serve
Environment=DFRELAY_SERVICE=1
Environment=DFRELAY_SERVICE_MANAGER=systemd-user
Environment=DFRELAY_SERVICE_UNIT=dfrelay.service
Restart=on-failure
RestartSec=3
TimeoutStopSec=10
KillMode=mixed
LimitNOFILE=4096

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now dfrelay.service
loginctl enable-linger "$USER"   # 可选：无需交互登录即可启动
systemctl --user restart dfrelay.service
systemctl --user stop dfrelay.service
```

若 `dfr` 不在 `/usr/local/bin`，请调整 `ExecStart`；`DFRELAY_SERVICE_UNIT` 必须与
unit 文件名一致。`dfr stop` 只停止当前服务，不会取消 enable；需要跨登录保持停止时
执行 `systemctl --user disable --now dfrelay.service`。也可用 `User=` 和显式
`HOME` 配置系统级服务；推荐用户服务。

### macOS launchd（LaunchAgent）

文件：`~/Library/LaunchAgents/io.github.fynhq.dfrelay.plist`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>io.github.fynhq.dfrelay</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/dfr</string>
    <string>serve</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>DFRELAY_SERVICE</key>
    <string>1</string>
    <key>DFRELAY_SERVICE_MANAGER</key>
    <string>launchd-gui</string>
    <key>DFRELAY_SERVICE_UNIT</key>
    <string>io.github.fynhq.dfrelay</string>
    <key>DFRELAY_SERVICE_TARGET</key>
    <string>/Users/YOUR_USER/Library/LaunchAgents/io.github.fynhq.dfrelay.plist</string>
    <key>HOME</key>
    <string>/Users/YOUR_USER</string>
    <key>PATH</key>
    <string>/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>ExitTimeOut</key>
  <integer>20</integer>
  <key>AbandonProcessGroup</key>
  <false/>
  <key>WorkingDirectory</key>
  <string>/Users/YOUR_USER</string>
</dict>
</plist>
```

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.fynhq.dfrelay.plist
launchctl kickstart -k gui/$(id -u)/io.github.fynhq.dfrelay
launchctl bootout gui/$(id -u)/io.github.fynhq.dfrelay
```

替换 `YOUR_USER` 并调整二进制路径与 target。`DFRELAY_SERVICE_UNIT` 必须与 plist
`Label` 一致，`DFRELAY_SERVICE_TARGET` 必须指向该 plist。LaunchAgent 在登录时启动。
`dfr stop` 使用 `launchctl bootout`，plist 仍在，下次登录会重新加载；target 路径用于
停止后再次 bootstrap。系统级 LaunchDaemon（`/Library/LaunchDaemons`）不受
DFRelay 自动控制。plist 不配置 stdout/stderr 重定向，因为 `dfr serve` 自己把
日志按天写入数据目录，重定向文件只会是空的。

## 安全

- 代理默认只监听回环；局域网访问需显式开启，开启后监听 IPv4 `0.0.0.0`。同时设置
  局域网用户名和密码后启用 HTTP/SOCKS 认证；认证只做访问控制，不加密流量。
- 私有、链路本地、ULA 等目标默认直连。
- 订阅 URL 与节点凭据按密钥处理；配置、缓存、日志使用私有权限。DFRelay 不修改系统
  DNS，也不自动设置系统代理。

## 第三方软件

DFRelay 不内嵌 sing-box。官方二进制单独下载，按 GPL-3.0-or-later 使用并保存在
DFRelay 数据目录；不支持外部自定义路径。

## 许可证

基于 MIT 许可发布，见 [LICENSE](LICENSE)。
