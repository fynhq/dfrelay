<a id="dfrelay-en"></a>

# DFRelay

DFRelay is a terminal UI proxy manager for Linux, macOS, and Android Termux
(arm64). It runs the official [sing-box](https://github.com/SagerNet/sing-box)
as an external process and exposes an HTTP/SOCKS mixed proxy on loopback by
default.

English | [中文](#dfrelay-zh)

## Features

- Loopback HTTP/SOCKS mixed proxy (`127.0.0.1:12000`, plus `[::1]:12000` when
  available); optional LAN access with HTTP/SOCKS authentication.
- Clash/Mihomo YAML, Base64 URI lists, sing-box JSON, and plain URI
  subscriptions; automatic latency-based selection, keyword filters, manual
  switching, and failed-node recovery.
- TUI with live traffic, connection counts, current node, local/proxy IP, logs,
  and a sing-box download/update page.
- Headless daemon for systemd/launchd, with foreground `dfr serve`; the TUI is a
  client and never owns the daemon lifecycle.

## Platforms

64-bit Linux (x86_64 and aarch64, musl), macOS (Intel and Apple Silicon), and
Android Termux arm64. **There is no TUN/VPN mode, automatic system-proxy
configuration, GUI/web UI, or remote control.**

## Resource usage

These are real-device measurements of resource usage while the programs are
resident, provided for reference. They exclude the TUI client and may vary with
subscriptions, node count, active connections, and logs.

| Platform | `dfr` | `sing-box` | Total |
|---|---:|---:|---:|
| Linux x64 | 5 MB | 27 MB | 32 MB |
| macOS 26 x64 | 6 MB | 26 MB | 32 MB |
| Android 12 arm64 | 5 MB | 28 MB | 33 MB |

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/fynhq/dfrelay/main/install.sh | sh
```

The installer downloads the latest release, verifies it against `SHA256SUMS`,
and installs `dfr` in place. Re-running it upgrades the existing installation.
If the target directory is not writable, it stops and asks you to rerun it with
`sudo`; it never invokes `sudo` itself.

On Android Termux (arm64), install the required tools first:

```bash
pkg install curl tar coreutils
```

A release archive contains `dfr`, `install.sh`, and license notices. You can run
`./install.sh`, install the binary manually, or run `./dfr` from the extracted
directory. `dfr` is a short name and may collide with another tool; use
`dfr --version` or the full path when in doubt.

| Target | Archive |
|---|---|
| Linux x86_64 musl | `dfrelay_linux_x64_v<version>_musl.tar.gz` |
| Linux aarch64 musl | `dfrelay_linux_arm64_v<version>_musl.tar.gz` |
| Android arm64 (Termux) | `dfrelay_android_arm64_v<version>.tar.gz` |
| macOS x86_64 | `dfrelay_macos_x64_v<version>.tar.gz` |
| macOS arm64 | `dfrelay_macos_arm64_v<version>.tar.gz` |

## Usage

```bash
dfr                 # attach the TUI; the daemon must already be running
dfr start           # start an ad-hoc daemon, or the configured service
dfr stop            # stop an ad-hoc daemon, or the configured service
dfr restart         # restart an ad-hoc daemon, or the configured service
dfr status          # show daemon mode, proxy state, node, and listen address
dfr serve           # run the daemon in the foreground for systemd/launchd
```

`dfr` is a pure client. If the daemon is not running, start it with `dfr start`
or with your service manager. `q` / `Ctrl-C` only exit the TUI; the daemon,
proxy, subscriptions, and logs keep running.

On first use, run `dfr start`, then `dfr`; install sing-box from Setup, add a
subscription with `n`, select nodes, and start the proxy with `s`. Point
applications at `http://127.0.0.1:12000` or `socks5://127.0.0.1:12000`.

The main keys are `q` (exit TUI), `s` (start/stop proxy), `R` (restart proxy,
twice), `L` (language), `?` (help), `h`/`l` or the arrow keys (pages), and
`j`/`k` or the arrow keys (lists and scrolling). Help lists page-specific keys.

When the daemon is service-managed and its service identity is configured,
`dfr start`, `dfr stop`, and `dfr restart` invoke the supervisor when the daemon
is stopped; `dfr start` on a running daemon only asks it to start the proxy.
Without the identity they refuse and print the command to run. `dfr stop` stops
the current instance but does not disable login or boot start. System-level services require
root; DFRelay prints the command and never runs `sudo` automatically. See
[Service templates](#service-templates).

On Android/Termux, only programs inside Termux or apps with a manual HTTP/SOCKS
proxy can use DFRelay. Run `termux-wake-lock` while keeping the daemon in the
background. There is no VPN or Android system-wide proxy.

## Configuration and logs

Linux, macOS, and Termux use XDG paths:

```text
$XDG_CONFIG_HOME/dfrelay/config.toml   # fallback: ~/.config/dfrelay/config.toml
$XDG_DATA_HOME/dfrelay/                # fallback: ~/.local/share/dfrelay/
```

The data directory contains the managed sing-box binary, subscription cache,
runtime configuration, inbox, and daily logs (kept for 7 days). Most settings
are in the TUI. Port, DNS, test URL, sing-box log level, and `bypass_lan` are
edited only in `config.toml`; restart the daemon after changing them. For an
ad-hoc daemon use `dfr restart`; for a service use `dfr restart` (which invokes
the supervisor) or the service manager.

The managed sing-box binary can be downloaded or updated from Setup. The
previous binary is kept for rollback. `--config` and `--data-dir` override the
XDG paths.

<a id="service-templates"></a>

## Service templates

Run DFRelay once with `dfr start` + `dfr` to install sing-box and configure
subscriptions before enabling a service. `dfr serve` starts the proxy
automatically when the configuration is valid. Choose one supervisor below; do
not enable both. With `DFRELAY_SERVICE=1` and the service identity below,
`dfr start/stop/restart` invoke the supervisor when the daemon is stopped;
`dfr start` on a running daemon only asks it to start the proxy. Without the
identity they refuse and print the supervisor command. Keep the same `--data-dir` for the
service and the CLI so both use the same instance marker. Existing units that
only set `DFRELAY_SERVICE=1` keep the old refusal behavior; add the identity
and restart the service once before relying on automatic control. A recorded
marker never falls back to an ad-hoc daemon: if you remove the service, also
remove `<data-dir>/service.json` before using `dfr start` ad hoc.

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

Adjust `ExecStart` if `dfr` is installed outside `/usr/local/bin`. The CLI uses
`systemctl --user` for the unit named above; keep `DFRELAY_SERVICE_UNIT` equal to
the unit file name. `dfr stop` stops the service now
but leaves it enabled; run `systemctl --user disable --now dfrelay.service` to
keep it stopped across logins. A system-wide service is also possible with
`User=` and an explicit `HOME`; it requires root and is not controlled
automatically by `dfr`. A user service is recommended for per-user
configuration.

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
target in `DFRELAY_SERVICE_UNIT`/`DFRELAY_SERVICE_TARGET` equal to the plist
`Label` and path. A LaunchAgent starts at login. `dfr stop` uses
`launchctl bootout`; the plist remains and is loaded again at the next login. The target path is required to bootstrap the job again
after a stop. A system-wide LaunchDaemon in `/Library/LaunchDaemons` requires
root and is not controlled automatically by `dfr`. The plist does not set
`StandardOutPath`/`StandardErrorPath`: `dfr serve` writes its own daily logs
to the DFRelay data directory, so redirected stdout/stderr files would stay
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
- GitHub mirrors are only transport for the official sing-box asset; the
  official release digest is the trust anchor.

## Third-party software

DFRelay does not bundle sing-box. The official sing-box binary is downloaded
separately under GPL-3.0-or-later and stored in the DFRelay data directory; a
custom external sing-box path is not supported.

## License

Released under the MIT license. See [LICENSE](LICENSE).

---

<a id="dfrelay-zh"></a>

# DFRelay

DFRelay 是一个面向 Linux、macOS 与 Android Termux（arm64）的终端 TUI
代理管理器。它以外部进程方式运行官方
[sing-box](https://github.com/SagerNet/sing-box)，默认在本机回环地址提供 HTTP/SOCKS
二合一代理。

[English](#dfrelay-en) | 中文

## 功能

- 回环 HTTP/SOCKS 二合一代理（`127.0.0.1:12000`，IPv6 可用时同时监听
  `[::1]:12000`）；可选局域网访问与 HTTP/SOCKS 认证。
- 订阅：Clash/Mihomo YAML、Base64 URI 列表、sing-box JSON、纯 URI；支持延迟自动选路、
  关键词过滤、手动切换和失败节点恢复。
- TUI：网速、连接数、当前节点、本机/代理 IP、日志，以及 sing-box 下载/更新页。
- 无界面 daemon 可由 systemd/launchd 托管；TUI 只是客户端，不负责 daemon 生命周期。

## 平台

64 位 Linux（x86_64、aarch64，musl）、macOS（Intel、Apple Silicon）、Android
Termux arm64。**不提供 TUN/VPN、系统代理自动设置、GUI/Web 界面或远程控制。**

## 资源占用

以下是真机测试程序常驻时的资源情况，仅供参考。不含 TUI 客户端，可能会随订阅、
节点数量、活动连接和日志量变化。

| 平台 | `dfr` | `sing-box` | 合计 |
|---|---:|---:|---:|
| Linux x64 | 5 MB | 27 MB | 32 MB |
| macOS 26 x64 | 6 MB | 26 MB | 32 MB |
| Android 12 arm64 | 5 MB | 28 MB | 33 MB |

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/fynhq/dfrelay/main/install.sh | sh
```

脚本下载最新发布包，用 `SHA256SUMS` 校验后原位安装 `dfr`；重复执行即升级。
目标目录不可写时脚本会停止并提示用 `sudo` 重新执行，脚本自身不调用 `sudo`。

Android Termux（arm64）先安装依赖：

```bash
pkg install curl tar coreutils
```

发布包包含 `dfr`、`install.sh` 和许可证说明。可以运行 `./install.sh`、手动安装二进制，
或直接在解压目录运行 `./dfr`。`dfr` 是短命令名，可能与其他工具冲突；用
`dfr --version` 或完整路径确认。

| 目标 | 发布包 |
|---|---|
| Linux x86_64 musl | `dfrelay_linux_x64_v<version>_musl.tar.gz` |
| Linux aarch64 musl | `dfrelay_linux_arm64_v<version>_musl.tar.gz` |
| Android arm64（Termux） | `dfrelay_android_arm64_v<version>.tar.gz` |
| macOS x86_64 | `dfrelay_macos_x64_v<version>.tar.gz` |
| macOS arm64 | `dfrelay_macos_arm64_v<version>.tar.gz` |

## 使用

```bash
dfr                 # 连接 TUI；daemon 必须已经运行
dfr start           # 后台启动 ad-hoc daemon，或启动已配置的系统服务
dfr stop            # 停止 ad-hoc daemon，或停止已配置的系统服务
dfr restart         # 重启 ad-hoc daemon，或重启已配置的系统服务
dfr status          # 查看模式、代理状态、当前节点和监听地址
dfr serve           # 前台运行 daemon，供 systemd/launchd 托管
```

`dfr` 只是客户端。daemon 没运行时应执行 `dfr start` 或通过服务管理器启动。
`q` / `Ctrl-C` 只退出界面，daemon、代理、订阅和日志继续运行。

首次使用先执行 `dfr start`，再执行 `dfr`；在 Setup 页安装 sing-box，按 `n` 添加订阅，
选择节点后按 `s` 启动。应用代理指向 `http://127.0.0.1:12000` 或
`socks5://127.0.0.1:12000`。

主要键位：`q` 退出界面，`s` 启动/停止代理，`R` 重启代理（两次确认），`L` 切换语言，
`?` 帮助，`h`/`l` 或方向键切页，`j`/`k` 或方向键移动/滚动。Help 页列出各页键位。

服务托管模式（`DFRELAY_SERVICE=1`）且配置了服务身份后，daemon 停止时
`dfr start`、`dfr stop`、`dfr restart` 会自动调用 systemd/launchd；daemon 运行中
`dfr start` 只请求启动代理。未配置身份时拒绝并打印手动命令。
`dfr stop` 只停止当前实例，不取消 enable/登录自启。系统级服务需要 root，DFRelay
不会自动执行 `sudo`，只打印命令。模板见 [服务模板](#service-templates-zh)。

Android/Termux 只提供本地代理：只有 Termux 内程序或手动设置 HTTP/SOCKS 代理的 App
可用；没有 VPN 或 Android 全局代理；后台运行时建议执行 `termux-wake-lock`。

## 配置与日志

Linux、macOS 与 Termux 遵循 XDG：

```text
$XDG_CONFIG_HOME/dfrelay/config.toml   # 兜底：~/.config/dfrelay/config.toml
$XDG_DATA_HOME/dfrelay/                # 兜底：~/.local/share/dfrelay/
```

数据目录包含受管 sing-box、订阅缓存、运行时配置、inbox 和按天日志（保留 7 天）。
大部分设置可在 TUI 修改；端口、DNS、测试 URL、sing-box 日志级别和 `bypass_lan`
只能编辑 `config.toml`，改后需要重启 daemon：ad-hoc 用 `dfr restart`，服务模式用
`dfr restart`（自动调用服务管理器）或服务管理器。受管 sing-box 可在 Setup 页下载/
更新，上一版保留用于回滚。
`--config`、`--data-dir` 可覆盖 XDG 路径。

<a id="service-templates-zh"></a>

## 服务模板

建议先用 `dfr start` + `dfr` 完成 sing-box 安装与订阅配置，再启用服务。配置正常时
`dfr serve` 会自动启动代理。下面二选一，不要同时启用。设置
`DFRELAY_SERVICE=1` 并配置下面的服务身份后，daemon 停止时
`dfr start/stop/restart` 会自动调用服务管理器；daemon 运行中 `dfr start` 只请求
启动代理。未配置身份时拒绝并打印服务管理器命令。服务与 CLI 应使用相同的
`--data-dir`，以读取同一实例的 marker。仅设置 `DFRELAY_SERVICE=1` 的旧 unit 仍保持
拒绝行为；请补齐身份并重启一次服务，再使用自动控制。marker 存在时不会回退 ad-hoc：
如果移除了服务，请先删除 `<data-dir>/service.json`，再使用 `dfr start`。

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

若 `dfr` 不在 `/usr/local/bin`，请调整 `ExecStart`。CLI 使用上面配置的 unit 名
执行 `systemctl --user`；`DFRELAY_SERVICE_UNIT` 必须与 unit 文件名一致。`dfr stop` 只停止当前服务，不会取消 enable；需要跨登录保持
停止时执行 `systemctl --user disable --now dfrelay.service`。也可用 `User=` 和显式
`HOME` 配置系统级服务；系统级服务需要 root，DFRelay 不会自动调用，按用户配置的
场景推荐用户服务。

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
停止后再次 bootstrap。系统级 LaunchDaemon（`/Library/LaunchDaemons`）需要 root，
DFRelay 不会自动控制。plist 不配置 `StandardOutPath`/`StandardErrorPath`：
`dfr serve` 自己把日志按天写入 DFRelay 数据目录，重定向出来的 stdout/stderr
文件只会是空的。

## 安全

- 代理默认只监听回环；局域网访问需显式开启，开启后监听 IPv4 `0.0.0.0`。同时设置
  局域网用户名和密码后启用 HTTP/SOCKS 认证；认证只做访问控制，不加密流量。
- 私有、链路本地、ULA 等目标默认直连。
- 订阅 URL 与节点凭据按密钥处理；配置、缓存、日志使用私有权限。DFRelay 不修改系统
  DNS，也不自动设置系统代理。
- GitHub 反代仅作为官方 sing-box 资产的传输通道，校验使用官方 release digest。

## 第三方软件

DFRelay 不内嵌 sing-box。官方 sing-box 二进制单独下载，按 GPL-3.0-or-later
使用并保存在 DFRelay 数据目录；不支持外部自定义路径。

## 许可证

基于 MIT 许可发布，见 [LICENSE](LICENSE)。
