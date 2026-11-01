# 双向剪贴板（v6）

支持 macOS / Windows 用户会话之间的纯文本与单张位图图片复制。网络格式为 UTF-8 和 PNG，保留 Unicode、多行文本及图片透明度。文件列表、文件夹、HTML/RTF、动画图片和剪贴板历史不在首版范围内；复制图片文件不会变成文件传输。

## 开关与使用

`serve` 和 `connect` 均接受 `--clipboard off|both|send|receive`，默认 both，方向相对运行命令的机器。双方权限取交集。只观看、诊断和保存单帧不启用剪贴板。

连接时只记下本端剪贴板版本，不读出和发送已有内容。从连接后下一次复制开始同步；锁屏暂停，恢复时重新建立版本基线，不补发暂停期间的内容。系统剪贴板属于整个用户会话，Mac 单窗口共享也可能同步其他应用复制的内容。

复制和粘贴沿用目标系统快捷键：Windows Ctrl+C/V，Mac Command+C/V。现有 Command ↔ Windows 键映射不变。客户端向远端发送 Ctrl/Command+V 前等待本地剪贴板版本被确认，最多 10 秒；等待期间保持键鼠连接心跳。超时取消该次粘贴并提示，键鼠连接继续使用。右键菜单、Shift+Insert 及本地目标应用粘贴不会被此等待机制拦截，应在图片同步完成后操作。不支持内容及超限内容不发送，重新复制支持的内容可继续使用。

## 通道与权限

剪贴板使用第三条独立的双向 TLS 连接，同一监听端口、设备身份和 SOCKS5 路由，无新增端口或证书操作。大图片不进入键鼠队列或视频流。

取得独占键鼠控制权后，`control_ready` 返回一个随机 256 位 `clipboard_token`。剪贴板连接经过 Hello/Welcome v6 后发送：

```json
{"type":"start_clipboard","token":"<64 hex characters>","send":true,"receive":true}
```

服务端验证令牌属于仍有效的控制会话且未被使用，返回 `clipboard_ready`，send/receive 表示客户端实际允许的方向。每个控制会话只能附加一次剪贴板连接。控制会话断开、超时或取消立即撤销令牌并取消剪贴板连接；只观看服务没有剪贴板后端。客户端关闭预览或画面断流时也取消剪贴板任务。剪贴板故障报告到终端，不自动重连或重放内容，键鼠可继续使用；重新连接可恢复剪贴板。

## 记录与仲裁

剪贴板连接沿用有界长度 JSON 消息，仅内容正文使用二进制：

1. `clipboard_offer`：revision、generation、format（text/png）、length。
2. 接收端校验大小、权限、可用桌面并记录本地剪贴板版本，返回 `clipboard_request`（revision）。
3. 发送端若该版本仍是最新待传版本，发送 `clipboard_data`（revision），紧接 offer.length 个原始字节；正文以 64 KiB 写入块发送。接收端完整读取正文后才解释下一条 JSON。
4. 写入前再次检查桌面和本地版本；成功返回 `clipboard_applied`（revision、generation），过期或冲突返回 `clipboard_rejected`。

revision 在各发送端递增，generation 由服务端仲裁。客户端提议携带最近看到的 generation，过期提议被拒绝，客户端按更新后的 generation 在后续轮询重试最新待传内容。服务端接受提议后递增 generation，并在允许服务端发送时回送权威版本，避免两端同时复制时各自停留在不同内容。两端复制的绝对时间不作比较。

每端仅保留一个最新待传内容和一个接收状态，不排队发送历史。读取独立运行，定时器不会取消半条记录的读取。二进制正文最多同时有一个待读取事件和一个处理事件，内存有界。使用每 2 秒 ping/pong 保活，单条消息/完整正文读写期限均为 10 秒，慢速代理传输大图可能超时。

本程序写入后的系统版本直接作为新基线，不再将自身写入作为新复制回传。传输期间用户又复制了内容，版本不匹配则不写入。锁屏时发送 `clipboard_paused`，丢弃待传和待接收状态。

## 原生适配与限制

原生读写由 Cargo.lock 锁定的 arboard 提供；macOS 用 NSPasteboard.changeCount，Windows 用 GetClipboardSequenceNumber，150 ms 轮询版本变化，仅变化时读取内容。读写与图片编码/解码放入阻塞工作线程，不占用预览窗口线程。Windows 原生格式转换由 arboard 提供 PNG / DIBV5 兼容输出。

- 文本最大 1 MiB，拒绝空值、非法 UTF-8 和 NUL；不转发富文本格式。
- PNG 最大 32 MiB；解码前校验尺寸，最多 16,777,216 像素；输出 RGBA 最多 64 MiB，解码器内部预算 128 MiB；拒绝动画 PNG。上述为单缓冲区预算，总进程内存还包括压缩数据、像素转换和系统库的临时内存。
- 内容不写日志或文件，不保存应用内历史；系统或第三方剪贴板历史仍由操作系统和用户设置管理。
- Windows 仅在当前输入桌面为 Default 时访问；锁屏 / UAC 安全桌面暂停。SYSTEM 模式的真实用户会话剪贴板访问和解锁恢复兼容性尚需实机验收，不能以普通用户测试代替。
- 只传单种内容，图片优先于文字；检测到文件列表时跳过。各应用自定义格式及“复制图片”的原生表示可能存在兼容差异。

## 验证

本机只运行 macOS 和共享模块的 Release 构建、测试和 Clippy，全部使用 `--locked`。自动化覆盖双向文字与大正文、无初始覆盖、防循环、同时复制收敛、本地复制竞争、暂停恢复、方向限制、令牌单次使用和撤权、PNG 透明度往返及非法/超限图片。

原生系统剪贴板测试默认 ignored，避免修改开发者的真实剪贴板。仅在一次性 GitHub Actions runner 设置 `REMOTEAPP_CLIPBOARD_NATIVE_TEST=1` 后显式运行，验证 Unicode 文本、图片原生读写和版本保护。Windows 只由 Windows runner 原生构建和验证。跨机器真实应用粘贴、SYSTEM 模式及图片兼容性仍需实机验收。
