# Windows 单进程锁屏实验

目标：管理员启动一个 `remote-cli.exe`，其他端观看锁屏画面并手动输入密码/PIN，在原登录会话恢复桌面。不安装系统服务，不创建辅助进程、计划任务或开机启动项，不修改系统登录策略。

**当前线程模拟方案的真实密码输入测试失败，不能用于远程解锁。** 默认 `serve` 不启用实验。截图及 SendInput API 返回成功，不能代表密码框能接收输入。现阶段请在 Windows 本机解锁后连接。

2026-10-30 用户反馈：可以看到已聚焦的密码输入框，但无法输入。随后把 CI 升级为真正的锁屏密码提交测试：仅在 GitHub-hosted 一次性 Windows runner 上设置随机临时测试密码，以当前输入工作线程提交一次，检查能否从 Winlogon 返回 Default。临时密码通过标准输入传递，不写入命令参数、环境变量或日志，不使用用户机器的密码。

- [真实输入测试 41079022375](https://github.com/wynemo/myremoteapp/actions/runs/41079022375/job/124143101329)：Release 构建、测试和 Clippy 通过；实际解锁失败。此版本已持续绑定输入线程桌面并使用物理扫描码，且一次性测试机已关闭 Ctrl+Alt+Del 要求以匹配密码界面。日志显示本程序进程完整性为 high，登录界面为 system，线程模拟为 SYSTEM；SendInput 接受但未完成解锁。
- [运行中提升进程完整性测试 41079800568](https://github.com/wynemo/myremoteapp/actions/runs/41079800568/job/124145564254)：Windows 返回错误 1375（令牌已作为主令牌使用），拒绝提升已运行进程的完整性级别。这段无效的启动修改已撤回，未作为可用修复交付。
- 程序未安装服务、未创建 SYSTEM 辅助进程。是否接受“启动时以 SYSTEM 身份自重启、原进程退出”的方案，正在等待用户确认，尚未实施。

下面的早期 API 探测记录只用于说明为什么需要完整解锁验收，不代表功能可用。

## 已完成的原生验证

2026-10-30，[Windows CI run 41075508137](https://github.com/wynemo/myremoteapp/actions/runs/41075508137/job/124130484034) 在代码提交 `0988d6f` 上完成 Release / `--locked` 构建、测试和 Clippy，锁屏探测也成功。进程 PID 始终为 2556：

| 实际桌面 | GDI 采集 | SYSTEM 线程身份 SendInput | 原进程身份 SendInput |
| --- | --- | --- | --- |
| Default | 1024×768，像素范围 0–255 | F24 松开事件接受数 1 | 接受数 1 |
| LockWorkStation 后的 Winlogon | 1024×768，像素范围 0–255 | F24 松开事件接受数 1 | 接受数 1 |

这证明本次 Windows runner 上单进程可以打开锁屏桌面、采集非纯色画面并调用输入 API。探测没有提交密码或 PIN，没有验证登录界面实际处理普通字符或完成解锁；不能把上述结果写成真实解锁验收通过。macOS 本机另已通过共享模块与 macOS Release 测试、Clippy。

## 实验命令（当前不能完成解锁）

在 Windows 项目目录下载这次已通过 CI 的实验版（默认下载 main 分支的命令不包含此实验）：

```powershell
python download_windows.py --run-id 41075508137 --force
```

在已登录的 Windows 上打开管理员 PowerShell：

```powershell
.\remote-cli.exe serve --desktop --experimental-lock-screen
```

复制程序自动生成的 `.local\host\connect.json` 到客户端，按原来的 `connect --address Windows的IP:9443` 连接。先确认普通桌面画面与输入，再在 Windows 本机按 Win+L，观察客户端是否继续显示锁屏。若过渡时断开，用同一文件重新连接。

只有能看见登录界面且键鼠实际有效时，才手动输入密码或 PIN 并提交一次。程序不提供自动填入、记住密码或重试。若黑屏、输入无效或报权限错误，在 Windows 本机解锁，保留不含密码的错误信息；不能根据未报错认定解锁成功。

验收还需检查：解锁后恢复原桌面、再次锁屏、锁屏前按住的键得到释放、断线/取消后释放输入，以及退出后无残留进程。要求 Ctrl+Alt+Del 的登录策略尚不支持；不处理开机登录、BitLocker、切换用户、跨登录会话或重启后的无人值守连接。UAC 虽可能使用同一个安全桌面，也尚未完成验收。

## 实现与权限边界

- 只在专用 OS 线程执行桌面操作。网络任务通过有界通道将输入交给输入线程，不在异步运行时线程上保留模拟身份。
- 管理员线程先 `ImpersonateSelf`，只给私有线程令牌启用 SeDebugPrivilege，避免修改进程共享令牌。查找同一会话的 winlogon，校验其令牌 SID 为 LocalSystem，再复制为受限句柄权限的模拟令牌并 `SetThreadToken`。
- 这不是修改进程主令牌。线程模拟是否足以通过 Windows 图形子系统和 UIPI，是本次实验要验证的问题。系统策略拒绝时明确报错，不自动改用辅助进程。
- 桌面操作用 `OpenInputDesktop` / `SetThreadDesktop`；只接受 Default，或 SYSTEM 线程上的 Winlogon。桌面句柄需包含 DESKTOP_JOURNALPLAYBACK 以用于输入。探测分别比较模拟身份和原进程身份的 SendInput 返回值；早期未申请该权限的实验在两种身份下均被拒绝，不能据此认定线程模拟不可行。操作结束恢复原桌面并关闭句柄；线程退出 `RevertToSelf`。恢复身份失败按微软要求终止进程。
- 切换桌面后清除旧帧坐标，拒绝基于上一个桌面画面的输入。采集与输入失败时尝试释放按键，维持原来的独占控制和 TLS 配对。

## Windows 原生探测

```powershell
.\remote-cli.exe windows-desktop-probe
# 下面的参数会真的锁定机器，不会自动解锁；仅在可本机恢复的测试机执行。
.\remote-cli.exe windows-desktop-probe --lock
```

探测输出同一 PID、线程有效身份、桌面绑定、采集尺寸与像素范围，以及模拟身份和原进程身份各一次 F24 松开事件的 SendInput 接受数。截图仅在内存中检查，不写入或上传。不会提交任何登录凭据。SendInput 返回 1 仅代表 API 接受，**不证明登录界面处理了输入，也不证明解锁成功**。

CI 在 Windows runner 原生进行 Release / `--locked` 构建、测试、Clippy；在其他 GUI 检查后运行锁屏探测。`scripts/test_windows_unlock.ps1` 仅允许在 GitHub-hosted 一次性 runner 运行，会给 runner 本地测试账号设置随机密码并在该测试机关闭 Ctrl+Alt+Del 要求；程序正常启动不修改账号或登录策略。`--ci-unlock` 从标准输入读取一次性测试密码，经过与远控相同的输入控制器提交一次，只有实际返回 Default 才标记 `actual_unlock_verified=true`。探测是允许失败的实验步骤，其结果必须单独查看，不可用整个工作流成功掩盖探测失败。macOS 本机只验证 macOS 与共享模块。

参考：[线程模拟](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setthreadtoken)、[恢复线程身份](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-reverttoself)、[桌面绑定](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop)、[SendInput 与 UIPI](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)。
