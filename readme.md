# MyRemoteApp

在 macOS 或 Windows 上观看并用鼠标操作 Mac 的指定 App 窗口，支持 SOCKS5 代理。

**当前支持点击、双击、拖拽和滚轮，键盘输入尚未实现。Mac 首次选择窗口时需要解锁。**

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

## 客户端：打开画面

Mac 在项目目录连接（将服务端的 `connect.json` 放在当前目录，替换为服务端 IP）：

```sh
cargo run --locked --release -- connect --address 192.168.1.10:9443
```

Windows：

两端都需更新；Windows 更新代码后运行 `python download_windows.py --force` 下载最新程序，再用 PowerShell 连接（替换为 Mac 的 IP）：

```powershell
.\remote-cli.exe connect --address 192.168.1.10:9443
```

使用 SOCKS5：

```powershell
.\remote-cli.exe connect --address 192.168.1.10:9443 --socks5 127.0.0.1:1080
```

连接后自动打开可用鼠标操作的窗口，按 `Esc` 或关闭窗口退出。代理需要认证时，设置 `REMOTEAPP_SOCKS5_USER` 和 `REMOTEAPP_SOCKS5_PASSWORD`。

鼠标操作会激活 Mac 上的目标窗口并影响本地鼠标。只观看时可在连接命令后加 `--view-only`。

Mac 按 `Ctrl+C` 停止共享；换窗口时重新运行并选择。Mac 锁屏时会暂停预览；再次连接时，在客户端终端输入服务端 Mac 的登录密码即可尝试解锁并恢复原窗口。密码不回显、不保存，也不自动重试。目标窗口关闭后需要重新选择。

服务必须已在 Mac 上启动；此功能不处理开机前的磁盘解锁。连接文件相当于访问密码；`serve --reset-pairing` 会让旧文件失效。更多用法见 [TECHNICAL.md](TECHNICAL.md)。
