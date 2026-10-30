# MyRemoteApp

在 macOS 或 Windows 上观看并用键盘和鼠标操作远端画面，支持 SOCKS5 代理。Mac 服务端共享指定 App 窗口；Windows 服务端共享整个桌面（包含所有显示器）。

**支持点击、双击、拖拽、滚轮、普通按键和组合键。Mac 首次选择窗口时需要解锁。**

## Mac：选择共享窗口

先解锁 Mac，在项目目录运行：

```sh
cargo run --locked --release -p remote-cli -- serve
```

首次使用需要允许屏幕录制和辅助功能权限，按提示授权后重新启动。程序会列出窗口，输入编号选择要共享的那个窗口。

小窗口文字不清楚时，可用独立 HiDPI 虚拟屏，让 App 放大后再采集：

```sh
cargo run --locked --release -- serve --virtual-display
```

此模式会移动并放大选中的窗口，正常停止共享时尝试恢复；不会修改主屏的分辨率。虚拟屏功能已实现，实际画质与系统兼容性尚待解锁后验证。

把 `.local/host/connect.json` 复制到客户端的当前目录即可；已经复制过且没有重置配对时，不用重复复制。

## Windows：共享整个桌面

在已登录且解锁的 Windows 桌面运行：

```powershell
.\remote-cli.exe serve --desktop
```

不加 `--desktop` 也默认共享整个桌面；只观看可加 `--view-only`。允许 Windows 防火墙访问监听端口 9443，将 `.local\host\connect.json` 复制到客户端当前目录，使用该 Windows 的 IP 连接。

需要锁屏输入时，改在管理员 PowerShell 运行 `serve --desktop --experimental-lock-screen`。程序会在新窗口以 SYSTEM 身份重启自身，原进程退出，运行中只保留一个远控进程；不安装系统服务。在新窗口按 Ctrl+C 停止。新版 EXE 和完整步骤见 [Windows 锁屏说明](docs/windows-lock-screen.md)，已通过 Windows CI 真实密码解锁测试，仍需目标设备复测。

默认模式遇到锁屏或 UAC 安全桌面会暂停。暂不支持 Windows 单窗口选择、Ctrl+Alt+Del 登录策略、BitLocker 或重启后自动运行；PIN 和多屏行为尚待实机验收。

## 客户端：打开画面

Mac 在项目目录连接（将服务端的 `connect.json` 放在当前目录，替换为服务端 IP）：

```sh
cargo run --locked --release -- connect --address 192.168.1.10:9443
```

Windows：

两端都需更新；Windows 更新代码后运行 `python download_windows.py --force` 下载最新程序，再用 PowerShell 连接（替换为服务端的 IP）：

```powershell
.\remote-cli.exe connect --address 192.168.1.10:9443
```

使用 SOCKS5：

```powershell
.\remote-cli.exe connect --address 192.168.1.10:9443 --socks5 127.0.0.1:1080
```

连接后自动打开可用键鼠操作的窗口；`Esc` 发送到远端，关闭窗口退出。代理需要认证时，设置 `REMOTEAPP_SOCKS5_USER` 和 `REMOTEAPP_SOCKS5_PASSWORD`。

Mac 的 Command 对应 Windows 键，Control 对应 Ctrl，Option 对应 Alt。键盘使用服务端布局与输入法；暂不转发本地输入法提交的中文文本或剪贴板，部分系统快捷键仍由本机处理。

键鼠操作会影响服务端本地输入；Mac 上还会激活目标窗口。只观看时可在连接命令后加 `--view-only`。

Mac 按 `Ctrl+C` 停止共享；换窗口时重新运行并选择。Mac 锁屏时会暂停预览；再次连接时，在客户端终端输入服务端 Mac 的登录密码即可尝试解锁并恢复原窗口。密码不回显、不保存，也不自动重试。目标窗口关闭后需要重新选择。

服务必须已在远端启动；Mac 解锁功能不处理开机前的磁盘解锁。连接文件相当于访问密码；`serve --reset-pairing` 会让旧文件失效。更多用法见 [TECHNICAL.md](TECHNICAL.md)。
