# MyRemoteApp

目标是在 Windows 上操作 Mac 的指定应用窗口，支持 SOCKS5 代理连接。

**项目尚未可用：目前只能检查网络连接，不能显示或操作 Mac 窗口。**

## 启动 Mac 服务

在项目目录运行：

```sh
cargo run --release -p remote-cli -- serve
```

首次启动会自动生成 `.local/host/connect.json`，把这个文件复制到 Windows 的 `remote-cli.exe` 旁边即可，无需手动生成、配置或交换证书。

## Windows 连接检查

从 [Actions](https://github.com/wynemo/myremoteapp/actions/workflows/ci.yml) 下载 Windows 构建，或运行 [下载脚本](scripts/download_windows.py)。解压后，在 PowerShell 中运行（替换为 Mac 的 IP）：

```powershell
.\remote-cli.exe connect --address 192.168.1.10:9443
```

使用 SOCKS5 代理：

```powershell
.\remote-cli.exe connect --address 192.168.1.10:9443 --socks5 127.0.0.1:1080
```

代理需要账号密码时，设置 `REMOTEAPP_SOCKS5_USER` 和 `REMOTEAPP_SOCKS5_PASSWORD` 环境变量。指定代理后不会回退直连；代理必须能够访问 Mac。

Mac 上按 `Ctrl+C` 停止服务。连接文件相当于访问密码；运行 `serve --reset-pairing` 后，旧文件失效，需要重新复制。

源码构建、下载脚本参数、代理认证示例及开发进度见 [TECHNICAL.md](TECHNICAL.md)。
