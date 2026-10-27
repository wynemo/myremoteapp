# 开发与使用说明

## 当前能力

- macOS / Windows 共用连接检查 CLI，支持 TCP 直连和 SOCKS5 CONNECT。
- SOCKS5 支持无认证及用户名/密码认证；域名交给代理解析；代理失败不回退直连。
- Mac 自动保存身份并导出连接文件，重启保持有效，`--reset-pairing` 撤销旧凭据。
- 内部保持 TLS 1.3 双向认证，用户不需要操作证书。连接文件包含访问凭据，不应公开或提交到 Git。
- Mac 使用 ScreenCaptureKit 枚举和捕获选定窗口；Windows 在独立原生窗口显示 JPEG 预览。
- 输出保持比例，最大 1280×720、10 FPS；慢连接只保留最新帧。当前不能注入键鼠。

尚未实现：H.264 硬件编解码、键鼠控制、WebRTC、代理 UDP 探测及传输切换。当前 JPEG 预览用于打通观看链路，不代表已完成目标中的 H.264 低延迟远控。

## 构建

固定使用 Rust 1.98.0，依赖由 Cargo.lock 锁定：

```sh
cargo build --locked --release -p remote-cli
cargo test --locked --release --workspace
cargo clippy --locked --release --workspace --all-targets -- -D warnings
```

Mac 构建需要 Xcode / Command Line Tools 的 Swift 工具链；发布二进制使用系统 Swift 运行库，用户无需安装 Swift。窗口采集需要 macOS 14+ 和屏幕录制权限。

Mac 本机构建产物为 `target/release/remote-cli`。本机不安装 Windows 工具链、不交叉编译 Windows；Windows 构建和测试交给 GitHub Actions 的 Windows runner，具体约定见 [AGENTS.md](AGENTS.md)。

## 连接文件与服务端

```sh
cargo run --locked --release -p remote-cli -- serve
```

默认列出可见的普通 App 窗口，输入编号后只采集选中窗口，不会自动切换到整个桌面或其他窗口。Mac 必须解锁；首次运行需在系统设置中授予当前终端或应用屏幕录制权限，可能需要退出终端再重试。

默认监听 `0.0.0.0:9443`，防火墙需允许此端口。可用 `--listen 127.0.0.1:9443` 限制本机访问。

默认状态目录是当前工作目录下的 `.local/host`，可通过 `--state-dir PATH` 修改。该目录保存：

- `identity.json`：Mac 本机身份和已授权客户端身份，留在 Mac。
- `connect.json`：导入 Windows 的连接凭据，复制这一个文件即可。

Unix 上新建状态目录权限为 0700，身份及连接文件权限为 0600。Windows 上文件访问权限继承所在目录，应保存在自己的用户目录中。连接文件为可重复使用的凭据，不是一次性验证码。

重启读取已有身份，不会自动撤销连接文件。凭据需要更换时，在停止旧服务后运行：

```sh
cargo run --locked --release -p remote-cli -- serve --reset-pairing
```

旧连接文件失效，需重新复制 `.local/host/connect.json`。现阶段多个客户端若导入相同文件会共享一个客户端身份；窗口控制权与逐设备授权尚未实现。

## 窗口选择与预览

```sh
# 查看可共享窗口及窗口 ID
cargo run --locked --release -p remote-cli -- windows

# 指定 ID，避免每次交互选择；窗口关闭重开后 ID 可能变化
cargo run --locked --release -p remote-cli -- serve --window-id 12345

# 保留纯网络诊断，不请求屏幕录制权限
cargo run --locked --release -p remote-cli -- serve --check-only
```

Windows 的 `connect` 默认打开可调整大小的原生预览窗口；仅观看，不回传键鼠。`Esc` 或关闭窗口退出。诊断用途可加 `--check-only`；Mac 没有图形客户端，可接收一帧验证传输：

```sh
target/release/remote-cli connect --address 127.0.0.1:9443 \
  --connection-file .local/host/connect.json --save-frame preview.jpg
```

`--save-frame` 不覆盖已有文件；也可结合 SOCKS5。不会在 CI 中抓取真实桌面或要求屏幕权限。Windows GUI 的源码编译由 Windows CI 验证，实际用户桌面的显示仍需在 Windows 使用时确认。

采集使用单窗口过滤，按初始窗口比例限制输出尺寸；调整目标窗口大小时保留输出画布，换窗口需要重启共享。最小化或隐藏窗口的行为依具体 App/macOS 而异，可能保留最后画面；关闭窗口或锁屏结束预览。程序不会自动切换采集目标。

初版预览复用同一条已认证 TLS 连接；客户端明确请求 `StartPreview` 后才开始发送画面，无需另开代理端口。JPEG 帧互相独立，可跳过积压的旧帧。每个接收端只持有最新帧，静止窗口每秒重发最后一帧作为保活。未来接入 H.264/键鼠时，再拆分视频和控制连接。

## SOCKS5

Windows PowerShell：

```powershell
.\remote-cli.exe connect --address 192.168.1.10:9443 --socks5 127.0.0.1:1080

# 代理需要认证时
$env:REMOTEAPP_SOCKS5_USER = "username"
$env:REMOTEAPP_SOCKS5_PASSWORD = "password"
.\remote-cli.exe connect --address mac.example:9443 --socks5 127.0.0.1:1080
```

`--socks5` 接受 `HOST:PORT` 或 `socks5://HOST:PORT`。支持 IPv4、IPv6（地址加方括号）和域名；目标域名由代理解析，代理本身的域名在本地解析。连接文件默认从当前目录读取，可用 `--connection-file PATH` 指定。

用户名和密码必须同时提供，也可使用 `--socks5-user` / `--socks5-password`，推荐环境变量以避免密码留在命令历史中。代理认证遵循 SOCKS5 协议，应用 TLS 保护的是 CONNECT 建立后的端到端数据，不是到代理的认证协商。

从 TCP 建连（或代理连接）到 TLS 握手的总期限为 10 秒；认证失败或代理不可达直接报错。该功能要求代理能够访问 Mac 的监听地址，不提供自动 NAT 穿透。

## 下载 Windows 产物

需要 Python 3.9+、GitHub CLI 和已登录的 GitHub 身份；也可设置 `GH_TOKEN`。不需要 Python 第三方依赖：

```sh
python3 download_windows.py
python3 download_windows.py --output ./downloads
python3 download_windows.py --run-id 40464225013 --force
```

Windows 上使用 `python` 或 `py -3` 替代 `python3`。默认读取 `wynemo/myremoteapp` 的 `main` 最近一次成功 push 构建，验证包的 SHA-256 及 PE x64 架构后，将 EXE 写到 项目根目录的 `remote-cli.exe`，不会运行它。旧 run（如上例）可能不支持新的连接文件用法，应优先下载最新构建。

已有文件默认不覆盖；`--force` 允许替换。`--repo OWNER/REPO`、`--branch BRANCH` 可修改来源；指定 `--run-id` 时使用该运行。产物保留 30 天，过期后不能从 Actions 下载。

## CI 与发布

[GitHub Actions](https://github.com/wynemo/myremoteapp/actions/workflows/ci.yml) 在分支推送、PR 和手动触发时执行：

| 平台 | Runner | Target | 打包格式 |
| --- | --- | --- | --- |
| Windows x64 | windows-2022 | x86_64-pc-windows-msvc | ZIP |
| macOS ARM64 | macos-14 | aarch64-apple-darwin | tar.gz |

两平台运行格式检查、Release Clippy、现有测试、Release 编译及二进制 `--help`。打包时校验 PE/Mach-O 架构，附带 SHA-256。日常构建在 Artifacts 下载；推送匹配 Cargo 版本的 `v*` 标签后自动发布 GitHub Release。当前包包含 Windows 原生观看窗口，但不能控制键鼠；macOS 二进制没有 Developer ID 签名或公证。

## 后续实现

1. VideoToolbox H.264 硬编与独立视频传输；Windows Media Foundation 解码和 D3D11 显示。
2. 键鼠输入、窗口焦点、坐标映射、断线释放按键。
3. WebRTC 优先、SOCKS5 UDP 能力探测与 TLS/TCP 传输切换。

Rust 承担共享协议、会话与传输，平台能力调用原生 API。窗口采集不等于独立后台桌面；操作可能影响 Mac 本地焦点。应用可达性与具体 App 的输入兼容性需逐项验证。

控制协议见 [docs/protocol.md](docs/protocol.md)。
