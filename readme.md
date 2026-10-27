# MyRemoteApp

在 Windows 上观看 Mac 的指定 App 窗口，支持 SOCKS5 代理。

**当前是窗口预览版：可以观看，暂不能操作键鼠。**

## Mac：选择共享窗口

先解锁 Mac，在项目目录运行：

```sh
cargo run --locked --release -p remote-cli -- serve
```

首次使用需要允许屏幕录制权限，按提示授权后重新启动。程序会列出窗口，输入编号选择要共享的那个窗口。

把 `.local/host/connect.json` 复制到 Windows 的 `remote-cli.exe` 旁边即可；已经复制过且没有重置配对时，不用重复复制。

## Windows：打开画面

更新代码后运行 `python download_windows.py --force` 下载最新程序，再用 PowerShell 连接（替换为 Mac 的 IP）：

```powershell
.\remote-cli.exe connect --address 192.168.1.10:9443
```

使用 SOCKS5：

```powershell
.\remote-cli.exe connect --address 192.168.1.10:9443 --socks5 127.0.0.1:1080
```

连接后自动打开预览窗口，按 `Esc` 或关闭窗口退出。代理需要认证时，设置 `REMOTEAPP_SOCKS5_USER` 和 `REMOTEAPP_SOCKS5_PASSWORD`。

Mac 按 `Ctrl+C` 停止共享；换窗口时重新运行并选择。锁屏或目标窗口关闭会结束当前预览，需要解锁或重新选择窗口。

连接文件相当于访问密码；`serve --reset-pairing` 会让旧文件失效。更多用法见 [TECHNICAL.md](TECHNICAL.md)。
