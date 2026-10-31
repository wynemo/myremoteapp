# Windows 锁屏输入与 SYSTEM 自重启

管理员启动锁屏模式时，程序以当前交互会话的 SYSTEM 身份重新启动自身，原进程立即退出。仅启动瞬间有两个 PID，运行中只保留一个远控进程。不注册 Windows 系统服务，不保留常驻辅助进程，不创建计划任务或开机启动项，不修改用户机器的登录策略或账号密码。

## 使用

先停止 Windows 上的旧程序，在管理员 PowerShell 下载已通过真实密码解锁验证的版本：

```powershell
python download_windows.py --run-id 41155922262 --force
.\remote-cli.exe serve --desktop --experimental-lock-screen
```

程序打开新的 SYSTEM 共享窗口，原进程退出。工作目录、启动参数和配对文件保持原样；已有客户端通常不用重新复制连接文件。客户端按原来的 `connect --address Windows的IP:9443` 连接。锁屏后，在远程画面的登录框中手动输入 Windows 密码并提交；程序不保存密码、不自动重试。正常解锁后会在同一预览连接自动恢复桌面，无需再运行 connect。在新的共享窗口按 Ctrl+C 停止。

不加 `--experimental-lock-screen` 时仍使用普通用户进程模式，遇到锁屏或 UAC 安全桌面暂停。要求 Ctrl+Alt+Del 的登录策略、Windows Hello/PIN、跨登录会话、退出登录、重启或 BitLocker 解锁不在本次验收范围内。该参数保留实验标记，仍需在目标 Windows 版本复测。

## 已完成的验证

[Windows CI run 41155922262](https://github.com/wynemo/myremoteapp/actions/runs/41155922262/job/124402571666) 在提交 `ca7f127` 上原生完成 Release / `--locked` 构建、测试和 Clippy；随后完成真实密码解锁：

1. 管理员进程创建 SYSTEM 替代进程，原进程退出，CI 明确检查原进程退出码。
2. 先通过一个预览协议会话订阅真实 Windows 采集流，再锁定当前会话，从 Default 切换到 Winlogon。
3. 在密码控件取得焦点后，通过与远控相同的输入控制器输入一次临时随机密码并提交。
4. 检查当前输入桌面实际返回 Default；报告 `actual_unlock_verified=true`，替代进程 PID 为 5548。
5. 解锁后原预览会话继续收到至少三帧更新序号的桌面画面；报告 `preview_continuity_verified=true`，全程未重连。

这不是只检查 SendInput 返回值。临时密码只用于一次性 GitHub-hosted runner，通过一次性双向命名管道传递，不放入命令参数、环境变量或日志。测试不使用用户机器的密码。

CI 测试脚本为一次性 runner 配置标准密码登录、可见测试账号并关闭强制自动登录与 Ctrl+Alt+Del 要求，使测试进入密码界面；这些操作只存在于 `scripts/test_windows_unlock.ps1`，普通程序启动不执行。CI 可采集输入密码前的界面及控件元数据，保留一天，不采集密码输入后的画面。UI Automation 仅用于测试准备用户名和密码框焦点，密码本身仍由实际输入控制器发送。

macOS 本机另已通过 macOS 与共享模块的 Release 测试、Clippy；未在 macOS 安装 Windows SDK 或交叉编译 Windows。目标用户设备、PIN、不同 Windows 版本及多屏实机行为不能用上述 CI 验证代替。

## 实现

- 在管理员启动线程临时模拟当前会话 SYSTEM 身份，校验令牌 SID，复制 SYSTEM 主令牌。启用私有线程令牌上的进程创建权限，使用 CreateProcessAsUserW 创建相同 EXE；新进程保留原参数和工作目录，使用 `winsta0\default` 及独立控制台。原进程立即返回，不维持辅助进程或父进程等待循环。SYSTEM 进程检查自身 SID，避免重复自重启。
- SYSTEM 锁屏模式下，桌面切换导致的短暂采集失败最多等待 5 秒：立即清除服务端旧画面和旧帧坐标、暂停输入，但不发送预览结束标记；新画面就绪后继续原连接。持续失败超过 5 秒仍按错误结束，避免无限等待。该期限短于客户端 10 秒画面超时。普通模式保持原来的停止行为。
- 采集与输入分别使用专用 OS 线程。输入线程持续绑定当前输入桌面，仅在桌面切换时重新绑定。清除上个桌面的帧坐标，拒绝旧画面事件；断开、失败和退出时尝试释放按键。
- 普通按键按前台键盘布局映射为物理扫描码，保留扩展键与松开标志；Pause 等特殊键使用虚拟键回退。客户端不提供 Unicode/IME 文本提交或剪贴板密码填入。

## 先前失败与验证边界

先前线程模拟版可以采集锁屏，SendInput 也返回 1，但用户反馈密码框无法输入。完整测试发现原进程完整性为 high、登录界面为 system；仅模拟线程身份不能作为输入可用的证明。运行中提高已使用主令牌的完整性被 Windows 以错误 1375 拒绝，该尝试已撤回。用户随后明确同意 SYSTEM 自重启方案。

早期 [API 探测 41075508137](https://github.com/wynemo/myremoteapp/actions/runs/41075508137) 仅验证 F24 松开事件被 API 接受，不代表密码框接收字符。CI 摘要必须区分构建检查和真实解锁结果；真实密码测试通过前，不可因整个工作流成功而声称解锁成功。

参考：[CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw)、[SendInput 与 UIPI](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)、[桌面绑定](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop)、[KEYBDINPUT 扫描码](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-keybdinput)。
