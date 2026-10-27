# MyRemoteApp

使用 Rust 开发 macOS → Windows 的单窗口远程控制工具。macOS 端捕获指定第三方 App 窗口并进行 H.264 硬件编码；Windows 端通过原生独立窗口显示画面并回传鼠标、键盘事件。计划支持 IP 直连、SOCKS5 代理、WebRTC 优先及 TLS/TCP 降级，不依赖第三方云服务。

## 当前状态

已实现第一步：**可运行的双向 TLS 控制通道诊断**，包含设备证书生成、双向身份校验、协议版本协商、ping/pong、消息长度限制、读写超时和连接数量限制。

尚未实现窗口采集、H.264 编解码、系统输入注入、Windows 原生界面、SOCKS5、WebRTC、视频传输与自动降级。当前命令行只验证控制通道，不代表已经可以远程控制窗口，也不测量画面或输入的端到端延迟。

按照 [AGENTS.md](AGENTS.md)，本机只验证 macOS；Windows 原生构建和测试由 GitHub Actions 的 Windows runner 执行，不在 Mac 上交叉编译。

## 架构与边界

```text
macOS                                      Windows
ScreenCaptureKit → VideoToolbox H.264       Media Foundation → D3D11
                        │                        ↑
                        └── WebRTC / TLS-TCP ────┘
CGEvent / Accessibility ← 输入与窗口状态 ← 原生窗口键鼠事件
```

- Rust 负责共享协议、会话、传输与平台模块编排；平台能力通过原生 API 接入。
- 首版只支持一个控制者操作一个选定窗口；暂不加入音频、文件传输和剪贴板。
- “无需公网 IP”要求直连路径、VPN 或 SOCKS5 代理到 Mac 的路径实际可达。不承诺在没有中继的任意 NAT 环境中连接成功。
- 单窗口采集不提供独立桌面会话。输入控制可能需要激活目标窗口，会影响 Mac 本地焦点；不承诺任意第三方 App 的完全后台操作。
- 控制和视频分别使用连接，避免视频在应用发送队列中的积压阻塞输入；TCP 本身的队头阻塞仍然存在。

## 工程结构

```text
crates/protocol      控制消息及有界帧读写
crates/session       握手与诊断会话
crates/transport     TLS 1.3、双向证书认证、ALPN
apps/remote-cli      init / serve / probe
```

后续加入 `host-macos` 与 `client-windows` 平台模块。当前两种平台的 CI 都构建共享诊断 CLI。

## 自动构建与下载

[GitHub Actions](https://github.com/wynemo/myremoteapp/actions/workflows/ci.yml) 在分支推送、Pull Request 和手动触发时执行两平台 Release 构建：

| 平台 | Runner | 目标 | 产物 |
| --- | --- | --- | --- |
| Windows x64 | windows-2022 | x86_64-pc-windows-msvc | ZIP，含 remote-cli.exe |
| macOS ARM64 | macos-14 | aarch64-apple-darwin | tar.gz，含 remote-cli |

两平台均固定 Rust 1.98.0，执行格式检查、Clippy、现有测试、Release 编译和原生二进制 `--help` 验证。打包前校验 PE/Mach-O 架构，生成 SHA-256 校验文件。构建产物保留 30 天，在对应运行页面的 Artifacts 下载。

推送与 Cargo.toml 版本一致的标签（如 `v0.1.0`）时，两平台构建成功后自动发布 [GitHub Release](https://github.com/wynemo/myremoteapp/releases)。日常分支推送只上传 Artifacts。当前下载包仅含命令行诊断工具和说明文档，尚未提供图形化远程控制客户端；macOS 包未做 Developer ID 签名及公证。

## 在 macOS 本机运行

需要支持 Rust 2024 edition 的 Rust/Cargo。仓库提交 Cargo.lock 固定本次验证的依赖版本。

```sh
cargo test --workspace
cargo build -p remote-cli
mkdir -p .local
cargo run -p remote-cli -- init --identity .local/host
cargo run -p remote-cli -- init --identity .local/client
```

在终端一启动服务端：

```sh
cargo run -p remote-cli -- serve \
  --listen 127.0.0.1:9443 \
  --identity .local/host \
  --peer-cert .local/client/cert.pem
```

在终端二探测：

```sh
cargo run -p remote-cli -- probe \
  --address 127.0.0.1:9443 \
  --identity .local/client \
  --peer-cert .local/host/cert.pem
```

成功时输出双向 TLS 认证结果、协议版本和控制通道 RTT。服务端使用 Ctrl-C 退出。

跨机器诊断时，两端各自运行 `init`，通过可信渠道交换 **cert.pem**，并分别通过 `--peer-cert` 指向对端证书。**key.pem 只能保留在所属设备**。现阶段使用手工证书交换，尚无图形化配对或自动信任首次连接。客户端不使用系统公共 CA，不提供跳过证书验证的开关。

服务端默认仅监听回环地址；局域网验证时显式设置 `--listen <Mac局域网IP>:9443`，并确保防火墙允许连接。身份目录必须不存在且父目录已经存在；`init` 不覆盖既有身份。macOS 上身份目录权限为 0700，密钥文件为 0600；`.local/` 已被 Git 忽略。证书使用固定协议服务名 `remoteapp.local`，连接地址可以是 IP，两者分开处理。

## 后续里程碑

1. **Mac 本地采集验证**：窗口枚举与选择、屏幕录制权限、指定窗口预览；验证遮挡、最小化、缩放及窗口关闭。
2. **硬件编码与视频通道**：VideoToolbox H.264 实时编码；设计独立视频连接、时间戳、尺寸版本、SPS/PPS 与 IDR 恢复；先在 Mac 本地验证码流。
3. **Windows 客户端源码**：Win32 窗口、Media Foundation 解码和 D3D11 渲染。本机编写和审查源码，通过 GitHub Actions Windows runner 原生构建与验证。
4. **输入闭环**：坐标映射、焦点校验、组合键、Unicode 文本、拖拽；断线释放按键；逐 App 验证。
5. **代理连接**：SOCKS5 CONNECT、认证、DNS 策略和 TLS 内层加密；UDP ASSOCIATE 单独探测。
6. **WebRTC 优先**：验证 Rust 库 H.264 与自定义网络 I/O；直接/代理 UDP 探测失败后切换 TLS/TCP，切换时从新 IDR 恢复。

协议细节见 [docs/protocol.md](docs/protocol.md)。

## 技术参考

- [Apple ScreenCaptureKit 示例](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)
- [Apple VideoToolbox 帧重排设置](https://developer.apple.com/documentation/videotoolbox/kvtcompressionpropertykey_allowframereordering)
- [Microsoft Media Foundation / D3D11 解码](https://learn.microsoft.com/en-us/windows/desktop/medfound/supporting-direct3d-11-video-decoding-in-media-foundation)
- [SOCKS5：RFC 1928](https://www.rfc-editor.org/rfc/rfc1928)
- [WebRTC 传输：RFC 8835](https://www.rfc-editor.org/rfc/rfc8835.html)
- [tokio-rustls](https://docs.rs/tokio-rustls/0.26/)
- [rcgen](https://docs.rs/rcgen/0.14/)
