# Windows 锁屏输入与 SYSTEM 自重启

管理员启动锁屏模式时，程序以当前交互会话的 SYSTEM 身份重新启动自身，原进程立即退出。仅启动瞬间有两个 PID，运行中只保留一个远控进程。不注册 Windows 系统服务，不保留常驻辅助进程，不创建计划任务或开机启动项，不修改用户机器的登录策略或账号密码。

## 使用

### 无驱动部署：HDMI 显示模拟器与控制台交接（实机通过）

2026-10-31，目标 Windows 实机已验证：保持 HDMI 显示模拟器插入，将 RDP 会话交给控制台后，本程序恢复连接；随后通过本程序锁屏、手动输入密码解锁，原窗口自动恢复画面，无需重连。该验证来自用户实际设备反馈，不是仅由 CI 推断。

从 RDP 的管理员 PowerShell 启动：

```powershell
.\remote-cli.exe serve --desktop --experimental-lock-screen
```

SYSTEM 替代进程在监听网络和创建采集线程之前，动态读取自己的会话 ID、显示协议和活动控制台 ID。已在控制台时直接继续；在 RDP 会话时，先锁定当前会话，通过 `WTSConnectSessionW` 将输出交给控制台，等待当前会话成为活动控制台，再确认锁屏后接受连接。RDP 会断开，原会话里的程序继续运行；无需手动执行 `query session` 或 `tscon`。该步骤不创建额外进程、服务、计划任务或驱动，也不读取用户密码。交接权限仅在自己的 SYSTEM 令牌临时启用，调用后恢复。

会话 ID 不固定；Session 0、未知协议、控制台切换中、交接失败或无法确认锁屏时明确报错，不启动共享。显示器或 HDMI 模拟器必须保持连接。普通模式不执行会话交接。

自动路径使用微软的 [WTSConnectSessionW](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsconnectsessionw)，对应此前已在目标机器验证的 `tscon.exe <当前RDP会话ID> /dest:console` 操作。此前的实机验收覆盖手动交接、锁屏、密码输入和原连接恢复；自动交接本身仍需目标 Windows 设备复测。Windows CI 验证控制台启动路径及真实密码解锁，不冒充 RDP 实机交接验收。

### 部署限制：无有效显示输出

已收到真实设备反馈：通过微软 RDP 启动后直接断开，Winlogon 上 BitBlt 返回 Win32=6。RDP 与控制台使用不同的显示输出；HDMI 模拟器需要配合会话交接，不能让程序留在断开的 RDP 会话。自动交接不会创建显示设备；完全没有实体或模拟显示器的场景仍不受支持。参考：[微软关于远程与控制台终端的说明](https://learn.microsoft.com/en-us/windows/win32/termserv/consoles-vs-terminals)。

### 在已有有效显示面的会话运行

先停止 Windows 上的旧程序，在管理员 PowerShell 下载已通过真实密码解锁验证的版本：

```powershell
python download_windows.py --run-id 41163225357 --force
.\remote-cli.exe serve --desktop --experimental-lock-screen
```

程序打开新的 SYSTEM 共享窗口，原进程退出。工作目录、启动参数和配对文件保持原样；已有客户端通常不用重新复制连接文件。客户端按原来的 `connect --address Windows的IP:9443` 连接。锁屏后，在远程画面的登录框中手动输入 Windows 密码并提交；程序不保存密码、不自动重试。正常解锁后会在同一预览连接自动恢复桌面，无需再运行 connect。在新的共享窗口按 Ctrl+C 停止。

不加 `--experimental-lock-screen` 时仍使用普通用户进程模式，遇到锁屏或 UAC 安全桌面暂停。要求 Ctrl+Alt+Del 的登录策略、Windows Hello/PIN、跨登录会话、退出登录、重启或 BitLocker 解锁不在本次验收范围内。该参数保留实验标记，仍需在目标 Windows 版本复测。

## 已完成的验证

自动交接版本提交 `1a8b56b` 已通过 [Windows CI run 41163225357](https://github.com/wynemo/myremoteapp/actions/runs/41163225357/job/124425141320)：原生 Release / `--locked` 构建、Clippy 和全部测试通过。SYSTEM 探针运行新的控制台准备入口后，完成真实密码解锁、锁屏新连接、原连接恢复及 BitBlt/CPU 缩放检查，四个报告字段均为 `true`。该 runner 位于控制台，未覆盖真实 RDP 到控制台的 API 分支。本机 macOS / 共享模块的 Release 测试和 Clippy 另行通过，未在 macOS 交叉编译 Windows。

以下保留此前采集与解锁验证的细节：

[Windows CI run 41157923196](https://github.com/wynemo/myremoteapp/actions/runs/41157923196/job/124408750699) 在提交 `757660b` 上原生完成 Release / `--locked` 构建、测试和 Clippy；随后完成真实密码解锁：

1. 管理员进程创建 SYSTEM 替代进程，原进程退出，CI 明确检查原进程退出码。
2. 先通过一个预览协议会话订阅真实 Windows 采集流，再锁定当前会话，从 Default 切换到 Winlogon。
3. 锁屏状态下再创建独立采集源和预览会话，确认新连接收到首帧；验证 `locked_connect_verified=true`。
4. 在密码控件取得焦点后，通过与远控相同的输入控制器输入一次临时随机密码并提交。
5. 检查当前输入桌面实际返回 Default；报告 `actual_unlock_verified=true`，替代进程 PID 为 7748。
6. 解锁后两个原预览会话均继续收到至少三帧更新序号的桌面画面；报告 `preview_continuity_verified=true`，全程未重连。

还在 Default 和 Winlogon 上强制执行原尺寸 BitBlt 加 CPU 缩小，并检查 JPEG 解码尺寸；报告 `native_copy_resize_verified=true`。这不是只检查 SendInput 返回值。临时密码只用于一次性 GitHub-hosted runner，通过一次性双向命名管道传递，不放入命令参数、环境变量或日志。测试不使用用户机器的密码。

CI 测试脚本为一次性 runner 配置标准密码登录、可见测试账号并关闭强制自动登录与 Ctrl+Alt+Del 要求，使测试进入密码界面；这些操作只存在于 `scripts/test_windows_unlock.ps1`，普通程序启动不执行。CI 可采集输入密码前的界面及控件元数据，保留一天，不采集密码输入后的画面。UI Automation 仅用于测试准备用户名和密码框焦点，密码本身仍由实际输入控制器发送。

macOS 本机另已通过 macOS 与共享模块的 Release 测试、Clippy；未在 macOS 安装 Windows SDK 或交叉编译 Windows。目标用户设备、PIN、不同 Windows 版本及多屏实机行为不能用上述 CI 验证代替。

## 实现

- 在管理员启动线程临时模拟当前会话 SYSTEM 身份，校验令牌 SID，复制 SYSTEM 主令牌。启用私有线程令牌上的进程创建权限，使用 CreateProcessAsUserW 创建相同 EXE；新进程保留原参数和工作目录，使用 `winsta0\default` 及独立控制台。原进程立即返回，不维持辅助进程或父进程等待循环。SYSTEM 进程检查自身 SID，避免重复自重启。
- SYSTEM 锁屏模式下，桌面切换导致的短暂采集失败最多等待 5 秒：立即清除服务端旧画面和旧帧坐标、暂停输入，但不发送预览结束标记；新画面就绪后继续原连接。持续失败超过 5 秒仍按错误结束，避免无限等待。该期限短于客户端 10 秒画面超时。普通模式保持原来的停止行为。
- 不需要缩放时直接 BitBlt，避免仍调用 StretchBlt。需要缩小时先走硬件 StretchBlt，失败后用原尺寸 BitBlt 读取，再用 CPU 双线性缩小至既定帧尺寸。原尺寸后备 DIB 上限为 256 MiB，防止超大虚拟桌面耗尽内存。原尺寸输出不额外复制像素。BitBlt 若仍失败，日志包含 Win32 错误码、桌面名称、源范围、输出尺寸及 RASTERCAPS；具体显卡兼容性仍须目标设备复测。
- 采集与输入分别使用专用 OS 线程。输入线程持续绑定当前输入桌面，仅在桌面切换时重新绑定。清除上个桌面的帧坐标，拒绝旧画面事件；断开、失败和退出时尝试释放按键。
- 普通按键按前台键盘布局映射为物理扫描码，保留扩展键与松开标志；Pause 等特殊键使用虚拟键回退。客户端不提供 Unicode/IME 文本提交或剪贴板密码填入。

## 先前失败与验证边界

先前线程模拟版可以采集锁屏，SendInput 也返回 1，但用户反馈密码框无法输入。完整测试发现原进程完整性为 high、登录界面为 system；仅模拟线程身份不能作为输入可用的证明。运行中提高已使用主令牌的完整性被 Windows 以错误 1375 拒绝，该尝试已撤回。用户随后明确同意 SYSTEM 自重启方案。

早期 [API 探测 41075508137](https://github.com/wynemo/myremoteapp/actions/runs/41075508137) 仅验证 F24 松开事件被 API 接受，不代表密码框接收字符。CI 摘要必须区分构建检查和真实解锁结果；真实密码测试通过前，不可因整个工作流成功而声称解锁成功。

参考：[CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw)、[SendInput 与 UIPI](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)、[桌面绑定](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop)、[KEYBDINPUT 扫描码](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-keybdinput)。
