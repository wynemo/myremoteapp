# 开发与使用说明

## 当前能力

- macOS / Windows 共用连接检查 CLI，支持 TCP 直连和 SOCKS5 CONNECT。
- SOCKS5 支持无认证及用户名/密码认证；域名交给代理解析；代理失败不回退直连。
- 服务端自动保存身份并导出连接文件，重启保持有效，`--reset-pairing` 撤销旧凭据。
- 内部保持 TLS 1.3 双向认证，用户不需要操作证书。连接文件包含访问凭据，不应公开或提交到 Git。
- Mac 使用 ScreenCaptureKit 枚举和捕获选定窗口；macOS / Windows 客户端在独立原生窗口显示 JPEG 预览。macOS 用 Core Animation 图像层按窗口 Retina 比例绘制，放大使用线性滤镜、缩小使用三线性滤镜，避免最近邻缩放把文字放大成像素块；鼠标按键继续由原生窗口处理；Mac 客户端直接将窗口鼠标位置转换到实际绘图视图的本地坐标，并读取该视图的实时尺寸，绘制与输入使用相同的等比例居中区域，不依赖 minifb 缓存的窗口尺寸或推算的内容区原点。
- 输出保持比例，按 Retina 像素采集，长边最大 2560、短边最大 1440（支持竖屏）、10 FPS；JPEG 质量 95、4:4:4 色度；慢连接只保留最新帧；窗口缩放或移到另一显示器后，采集尺寸自动跟随，客户端标题栏显示实际接收像素。1 倍缩放显示器上的小窗口仍只有其自身像素，需放大源窗口或使用 HiDPI 才能增加细节。支持鼠标左/右/中键、双击、拖拽及滚轮。

尚未实现：H.264 硬件编解码、本地输入法文本提交和剪贴板同步、WebRTC、代理 UDP 探测及传输切换。当前 JPEG 预览用于打通观看链路，不代表已完成目标中的 H.264 低延迟远控。

## Windows 完整桌面服务端

Windows 的 `serve` 默认共享整个虚拟桌面，`serve --desktop` 可显式选择；包含全部显示器及其负坐标布局。`--view-only` 禁用键鼠控制，`--check-only` 仍只做网络诊断；Windows 不接受 `--window-id` 或 `--virtual-display`。Mac 保持单窗口共享，暂不支持 `--desktop`。

独立线程使用 Per-Monitor V2 DPI 坐标、GDI 顶向下 32 位 DIB 采集完整桌面。无需缩放时使用 BitBlt 直接复制；需要缩小时优先 StretchBlt，驱动拒绝时回退为原尺寸 BitBlt 加 CPU 双线性缩小。后备原尺寸 DIB 限制为 256 MiB，采集失败记录 Win32 错误码、桌面和尺寸信息。保持比例缩小至长边不超过 2560、短边不超过 1440，不放大源图像；仅实际预览会话存续期间采集；启动待机和最后一个预览断开后不截图、不编码，采集线程每 100 ms 检查观看需求。诊断、状态查询、独立鼠标连接及源对象克隆不触发采集；预览失败或任务取消也会释放观看计数。最多 10 FPS，JPEG 95 / 4:4:4。使用 SRCCOPY，避免 CAPTUREBLT 在部分系统上反复隐藏/显示软件鼠标造成闪烁；部分分层或透明窗口可能无法完整采集。多屏总面积越大，每个屏幕在客户端中的有效像素越少。受保护视频或不支持 GDI 采集的内容可能为空白。GDI 对象仅在采集线程使用并按顺序释放；普通模式采集失败会结束预览；SYSTEM 锁屏模式对桌面切换的短暂采集失败提供最多 5 秒恢复时间：清除旧画面和帧坐标并暂停输入，不发送预览结束标记，新画面就绪后继续原连接。持续失败超过 5 秒仍报告停止，不无限隐藏故障。

复用现有持久配对、双向 TLS、SOCKS5、独立控制连接及会话心跳。SendInput 使用 ABSOLUTE / VIRTUALDESK 将客户端归一化坐标映射到完整桌面。保留最近 32 帧的物理桌面边界，显示器布局变化后拒绝旧帧操作；采集暂停、连接结束及服务关闭时尝试释放按键。仅一个客户端可取得键鼠控制。输入受 Windows UIPI 权限限制，不能控制权限更高的程序。

服务运行在当前交互会话，不注册 Windows 系统服务，不保留常驻辅助进程，不设置计划任务或开机启动。默认模式遇到锁屏或 UAC 安全桌面停止画面和控制。`serve --experimental-lock-screen` 需要管理员启动：通过当前会话的 SYSTEM 主令牌与 CreateProcessAsUserW 重新启动相同 EXE，保留原参数和工作目录，原进程立即退出。仅启动瞬间有两个 PID，随后只保留 SYSTEM 替代进程；在其新窗口按 Ctrl+C 停止。此启动方式已得到用户明确授权。

此前线程模拟版的进程完整性仍为 high，不能将桌面访问或 SendInput 接受等同于密码框可输入；运行中提高进程完整性也被 Windows 以错误 1375 拒绝。现采用 SYSTEM 主令牌启动，输入线程持续绑定当前桌面，桌面切换时重绑并丢弃旧帧。按键优先按前台布局映射为物理扫描码，保留扩展键与松开标志。Windows CI 已通过原进程退出、锁屏、实际输入一次临时密码、恢复 Default 桌面，并在同一预览会话继续收到新画面的完整验证；证据和限制见 [Windows 锁屏说明](docs/windows-lock-screen.md)。客户端终端不索取 Windows 密码；密码在远程画面中手动输入，不保存、不自动重试。PIN、Ctrl+Alt+Del 登录策略及不同 Windows 版本实机兼容性尚未涵盖。身份和 TLS 配置仍由程序处理，复制 `.local/host/connect.json` 即可。

验证边界：本机只验证 macOS 和共享模块，不安装 Windows SDK 或交叉编译工具。Windows 构建、测试及 Clippy 由 GitHub Actions 的 Windows runner 原生执行，使用 Release 模式和 `--locked`。真实 Windows 桌面的采集、多屏显示和鼠标操作仍需实机验收，不能以 CI 通过代替。

API 依据：[StretchBlt](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-stretchblt)、[SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)、[MOUSEINPUT 虚拟桌面坐标](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-mouseinput)。

## 构建

固定使用 Rust 1.98.0，依赖由 Cargo.lock 锁定：

```sh
cargo build --locked --release -p remote-cli
cargo test --locked --release --workspace
cargo clippy --locked --release --workspace --all-targets -- -D warnings
```

Mac 源码构建使用 Xcode 26.3 或更新版本，采集依赖需要 macOS 26 SDK；发布二进制使用系统 Swift 运行库，用户无需安装 Swift。当前运行验证环境为 macOS 26，旧版本 macOS 尚未验证。窗口采集需要屏幕录制权限。

原生客户端回归检查：`python3 scripts/smoke_viewer.py --binary target/release/remote-cli`。Windows CI 加 `--test-keyboard` 向合成预览窗口投递键盘消息并核对 TLS 事件顺序、重复键、Esc、Win 键与失焦释放；macOS 本机可加 `--test-keyboard --test-mouse`，在辅助功能权限允许时仅向测试预览窗口注入键鼠事件，再隐藏该测试窗口验证失焦释放。此检查曾复现旧版焦点判断反转及左右修饰键被同时按下的问题。

Mac 本机构建产物为 `target/release/remote-cli`。本机不安装 Windows 工具链、不交叉编译 Windows；Windows 构建和测试交给 GitHub Actions 的 Windows runner，具体约定见 [AGENTS.md](AGENTS.md)。

## 连接文件与服务端

```sh
cargo run --locked --release -p remote-cli -- serve
```

默认列出可见的普通 App 窗口，输入编号后只采集选中窗口，不会自动切换到整个桌面或其他窗口。Mac 首次选择窗口时必须解锁；首次运行需在系统设置中授予当前终端或应用屏幕录制权限，可能需要退出终端再重试。默认启用键鼠控制，还需辅助功能权限；Mac 首次选择窗口时需要解锁，运行中锁屏后可通过手动密码解锁。

默认监听 `0.0.0.0:9443`，防火墙需允许此端口。可用 `--listen 127.0.0.1:9443` 限制本机访问。

默认状态目录是当前工作目录下的 `.local/host`，可通过 `--state-dir PATH` 修改。该目录保存：

- `identity.json`：Mac 本机身份和已授权客户端身份，留在 Mac。
- `connect.json`：导入客户端的连接凭据，复制这一个文件即可。

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

macOS / Windows 的 `connect` 默认打开可调整大小的原生窗口，并建立独立的键鼠控制连接。`Esc` 发送到远端，关闭窗口退出。诊断用途可加 `--check-only`；macOS 和 Windows 均可接收一帧验证传输：

```sh
target/release/remote-cli connect --address 127.0.0.1:9443 \
  --connection-file .local/host/connect.json --save-frame preview.jpg
```

`--save-frame` 不覆盖已有文件；也可结合 SOCKS5。不会在 CI 中抓取真实桌面或要求屏幕权限；macOS / Windows CI 会通过合成 TLS 服务验证原生窗口的横竖屏绘制、控制连接心跳及断流后的输入释放，无需屏幕录制权限。真实桌面的显示和鼠标操作仍需实际连接确认。

采集使用单窗口过滤，保持比例限制输出尺寸；调整目标窗口大小或移动到另一显示器时自动更新输出尺寸，换窗口需要重启共享。最小化或隐藏窗口的行为依具体 App/macOS 而异，可能保留最后画面；关闭窗口或锁屏会结束当前预览；解锁后服务自动恢复原窗口，客户端重新连接即可继续。程序不会自动切换采集目标。

初版预览复用同一条已认证 TLS 连接；客户端明确请求 `StartPreview` 后才开始发送画面，无需另开代理端口。JPEG 帧互相独立，可跳过积压的旧帧。每个接收端只持有最新帧，静止窗口每秒重发最后一帧作为保活。鼠标使用另一条独立的双向 TLS 连接，同样遵循指定的 SOCKS5 路由；无需在 Mac 新开端口。

## 键鼠控制与权限

默认 `serve` 需要屏幕录制和辅助功能权限。缺少辅助功能权限时会请求系统提示，并要求在系统设置授权后重试。Mac 锁屏会暂停当前窗口共享；程序支持已登录用户会话的手动密码解锁，不提供开机登录、FileVault 磁盘解锁或独立的后台桌面。

```sh
# 服务端仅共享画面，不请求辅助功能权限
cargo run --locked --release -p remote-cli -- serve --view-only
```

客户端只观看时，在 `connect` 后加 `--view-only`。否则会用同一设备身份、同一 SOCKS5 路由，再建立一条 TLS 键鼠连接。一次只允许一个客户端获得键鼠控制，其他客户端可只观看。

坐标先去除客户端窗口的黑边，再用所显示帧的序号匹配 Mac 采集时的内容区域，转换到当前目标窗口位置。目标窗口调整尺寸后，旧帧上的点击会被拒绝，等待新画面即可重试；不会按旧比例误点。Mac 端在按下或滚轮前尝试激活目标窗口，并通过辅助功能命中测试确认目标位置；应用未返回完整辅助功能窗口信息时，补充查询系统窗口从前到后的层叠顺序，仅当点击位置最上层的窗口 ID 和进程都与共享目标一致时允许操作。若辅助功能已命中目标进程，但控件没有 `AXWindow`，则沿同一进程的辅助功能父级链最多检查 8 层，允许使用包含点击位置的父容器范围（叶子元素可能返回不包含该点的范围），遇到其他进程停止；同时在目标进程的窗口层叠中确认点击位置最前的窗口 ID 与共享目标一致；不再将控件尺寸与整个窗口尺寸比较。辅助功能已确认穿透的其他进程覆盖层不参与这一步的同应用窗口归属判断，同进程其他窗口仍会阻止错误点击。激活后等待最多约 500 ms 让窗口层叠稳定，不能确认时拒绝事件；错误提示不再将窗口信息缺失一律描述为遮挡。

点击被拒绝时，错误附带实际服务进程读取的辅助功能命中状态、命中元素及祖先的进程/坐标，以及系统窗口层叠顶部 ID。只读复查可在同一授权终端运行 `remote-cli input-diagnostics --window-id ID --pid PID --x 32767 --y 55000`（x/y 为 0–65535 的窗口归一化坐标）；命令不激活窗口或发送鼠标事件。SSH 进程的辅助功能授权可能与实际服务不同，不能用其结果替代服务内诊断。

左/右/中键、双击、拖拽与滚轮通过系统事件注入。客户端窗口失焦、移出画面、关闭窗口或正常断开时释放已按下的按钮；连接静默中断由心跳在约 10 秒内检测并释放。Mac 锁屏或采集结束也释放按钮。鼠标会影响 Mac 本地指针与目标窗口焦点，不提供与本地用户互不干扰的后台操作。

键盘使用有界命名键事件 `Key { frame_sequence, key, down }`，与鼠标共用有序控制连接；字母、数字、标点、导航、功能键、数字小键盘和左右修饰键分别映射。客户端使用原生事件回调，保留同一刷新周期内的快速按下/松开和重复事件；移动合并不会跨过键盘事件。Mac 本地监听器仅处理当前预览窗口，区分左右修饰键并将 Command 组合键绕过本应用的退出/隐藏菜单；不安装全局键盘钩子。Command 对应 Windows 左/右 Win 键，Control 对应 Ctrl，Option 对应 Alt；`Esc` 转发到远端，关闭窗口退出，系统保留快捷键仍可能由本机截获。

Windows 客户端以窗口子类回调补齐 minifb 缺失的 Win、左右 Alt 与 F13–F15 事件，仅监听当前预览窗口并在销毁前卸载。Windows 服务端用 SendInput 注入命名虚拟键，保留右 Ctrl/Alt、导航键、Win 键和数字小键盘 Enter 的扩展键标志；Mac 用 CGEvent 键码、修饰标志与重复标志向选定 App 进程发送事件，并在按下前确认共享窗口焦点。Mac 的 Menu、Pause、ScrollLock 没有对应键，拒绝这些输入。输入使用服务端键盘布局/输入法，不转发客户端 Unicode/IME 提交文本；可在服务端切换输入法后用字母键输入拼音，但不保证所有输入法和 App 行为一致。Windows 普通桌面上的实际键盘效果仍需实机验收；不提供锁屏密码输入或 Ctrl+Alt+Del 安全注意序列。

Windows 客户端通过系统前台窗口、可见及非最小化状态判断焦点，避免 minifb 的线程活动窗口状态在后台仍为真。失焦时单独发送 ReleaseKeyboard，移出鼠标画面时只释放鼠标；窗口关闭、断流、采集暂停及会话取消释放全部已注入的按键。先释放普通键，再释放修饰键；Windows 若系统拒绝松开则保留状态供恢复后重试。失焦发生在本地监听器之外，之后的松开事件收不到，因此重新聚焦时按物理键状态重新同步：仍被按住的键重新按下，已在别处松开的键直接丢弃。否则一个跨过焦点切换的修饰键会一直留在阻塞集合里，把之后所有按键都吞掉，只能断开重连才能恢复（macOS 用 CGEventSourceKeyState 读取 HID 状态，Windows 客户端用 GetAsyncKeyState；查不到时按已松开处理）。该同步由单元测试覆盖，真实键盘下的焦点往返仍需实机验收。`--view-only` 不建立控制连接，也不启用键盘捕获。

当前控制协议为 v5，旧程序不兼容，升级时需要同时更新客户端和服务端并重启；Windows 需下载新版 EXE；`connect.json` 不需要重新生成。

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

## 手动密码解锁

先在解锁的 Mac 上启动 `serve` 并选择窗口。之后 Mac 可以锁屏；服务继续运行，释放鼠标并丢弃旧画面。客户端再次运行原来的 `connect` 命令时，先通过双向 TLS 查询锁定状态：

- 已解锁：直接打开窗口，不索要密码。
- 已锁定且可解锁：终端提示输入一次 Mac 登录密码，不回显。无需额外命令或密码参数。
- 不支持解锁或缺少辅助功能权限：明确报错，不收集密码。

程序不使用钥匙串、不提供“记住密码”，Mac 登录密码不能通过命令行参数、环境变量或连接文件传入。密码通过既有 TLS/SOCKS5 路径的专用有界二进制记录传输，不进入 JSON 控制消息、日志或文件。Rust 接收/发送侧的临时密码缓冲区使用 zeroize 在释放时清理；系统认证界面的临时内存由 macOS 管理。

Mac 原生端确认当前控制台用户、已登录状态、Apple 签名的 loginwindow 进程及已识别的安全密码框，再设置该密码框并执行一次系统提交。不通过全局键盘事件输入密码；必要时只发送一次不含秘密的 Return 以展开锁屏界面。系统未解锁就返回失败，不自动重新提交；每分钟最多三次请求。连接结束会取消尚未提交的操作，失败会尝试清空原安全密码框。

观察到锁屏时记录采集重建请求，避免锁屏/解锁发生在维护轮询间隔内而继续复用旧流。解锁成功后服务重建之前选择的窗口采集；仅当新流已收到有效画面、未暂停且解锁状态在维护检查中持续稳定至少 1 秒，才报告恢复完成。启动采集流本身不再代表就绪，恢复期间重新锁屏会清除就绪状态并重新等待。窗口已关闭则提示在 Mac 重选，不改采其他窗口或整个桌面。`connect.json` 保持有效。`serve --view-only` 和 `serve --check-only` 不启用远程解锁。

验证边界：协议、取消、错误回执和终端不回显已用合成解锁后端验证；没有使用真实系统密码做锁屏解锁端到端验收。系统登录界面的版本差异可能导致明确返回“当前无法解锁”，需要实际设备验证。

## 下载 Windows 产物

需要 Python 3.9+、GitHub CLI 和已登录的 GitHub 身份；也可设置 `GH_TOKEN`。不需要 Python 第三方依赖：

```sh
python3 download_windows.py
python3 download_windows.py --output ./downloads
python3 download_windows.py --run-id 40464225013 --force
```

Windows 上使用 `python` 或 `py -3` 替代 `python3`。默认读取 `wynemo/myremoteapp` 的 `main` 最近一次成功 push 构建，验证包的 SHA-256 及 PE x64 架构后，将 EXE 写到项目根目录的 `remote-cli.exe`，不会运行它。旧 run（如上例）可能不支持窗口预览，应优先下载最新构建。

已有文件默认不覆盖；`--force` 允许替换。`--repo OWNER/REPO`、`--branch BRANCH` 可修改来源；指定 `--run-id` 时使用该运行。产物保留 30 天，过期后不能从 Actions 下载。

## CI 与发布

[GitHub Actions](https://github.com/wynemo/myremoteapp/actions/workflows/ci.yml) 在分支推送、PR 和手动触发时执行：

| 平台 | Runner | Target | 打包格式 |
| --- | --- | --- | --- |
| Windows x64 | windows-2022 | x86_64-pc-windows-msvc | ZIP |
| macOS ARM64 | macos-26（Xcode 26.3） | aarch64-apple-darwin | tar.gz |

两平台运行格式检查、Release Clippy、现有测试、Release 编译及二进制 `--help`。打包时校验 PE/Mach-O 架构，附带 SHA-256。日常构建在 Artifacts 下载；推送匹配 Cargo 版本的 `v*` 标签后自动发布 GitHub Release。当前包包含 macOS / Windows 原生窗口和键鼠控制；macOS 二进制没有 Developer ID 签名或公证。

## 后续实现

1. VideoToolbox H.264 硬编与独立视频传输；Windows Media Foundation 解码和 D3D11 显示。
2. 本地输入法文本提交和剪贴板同步。
3. WebRTC 优先、SOCKS5 UDP 能力探测与 TLS/TCP 传输切换。

Rust 承担共享协议、会话与传输，平台能力调用原生 API。窗口采集不等于独立后台桌面；操作可能影响 Mac 本地焦点。应用可达性与具体 App 的输入兼容性需逐项验证。

控制协议见 [docs/protocol.md](docs/protocol.md)。


## 独立 HiDPI 工作区

`serve --virtual-display` 在选择窗口后创建扩展显示器：1440×1080 逻辑点、2880×2160 物理像素，置于现有显示器右侧，不镜像或修改主显示器模式。通过辅助功能唯一匹配目标窗口，按原比例将窗口放大到不超过 1280×960 逻辑点，并移入虚拟屏；仅采集原先选择的窗口。

虚拟显示使用运行时检测的 CoreGraphics SPI。系统不支持、HiDPI 模式未生效、窗口无法移入虚拟屏时会明确报错并输出位置诊断；窗口限制调整大小时保留实际大小继续采集，并明确提示未达到期望放大尺寸，不会把旧的低分辨率帧放大后宣称高清。移动后重新读取 ScreenCaptureKit 的窗口元数据，等待真实的 2 倍像素比例生效，再开始采集；传输仍受长边 2560、短边 1440 的限制。

工作区独立于一次捕获流的生命周期，锁屏暂停和解锁重建捕获流不会销毁虚拟屏。Ctrl+C、SIGTERM 和正常退出时，先停止鼠标与捕获，再尝试恢复目标窗口原来的大小、位置并释放虚拟屏。窗口已关闭、系统拒绝辅助功能操作或强制杀死进程时，不能保证精确恢复窗口位置。

验证边界：代码完成编译和非图形测试；尚未在目标 Mac 上完成虚拟屏创建、App 移动、退出恢复及同花顺实际清晰度的验收。用户要求锁屏期间不进行图形化操作，因此不运行这些验证。解锁后应对比同花顺实际源画面与接收画面，不能以分辨率日志或 CI 通过代替清晰度验收。

创建 HiDPI 模式的参数关系参考 [Chromium 的虚拟显示器实现](https://chromium.googlesource.com/chromium/src/+/HEAD/ui/display/mac/test/virtual_display_util_mac.mm)：物理尺寸与逻辑尺寸分开配置，HiDPI 使用一半大小的逻辑模式。Cindy 的 `macos-viewer-display.m` 则使用虚拟屏镜像源显示器，本项目未采用镜像方式。

## 鼠标输入延迟

客户端发送前合并连续积压的鼠标移动，只保留最新坐标；点击、释放和滚轮事件作为顺序边界，不跨越合并。Mac 对每次点击和滚轮保留实际命中检查；目标 App 已激活且目标窗口无遮挡时跳过重复枚举、置前操作。未改变 10 FPS 采集上限，画面反馈仍包含采集、编码与传输延迟。队列顺序通过非图形测试验证，实际端到端延迟尚未测量。

客户端将压缩帧接收与 JPEG 解码分离：网络接收持续推进，待解码区仅保留最新一帧，解码在阻塞工作线程串行执行，避免慢解码让旧帧持续积压。非图形测试模拟解码阻塞，验证跳过中间帧及传递停止状态。服务端紧密排列的 BGRA 缓冲直接用于编码，仅对有行填充的缓冲重新打包；JPEG 质量及色度采样不变。
