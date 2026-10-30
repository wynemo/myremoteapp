# Windows 单进程锁屏实验

目标：管理员启动一个 `remote-cli.exe`，其他端观看锁屏画面并手动输入密码/PIN，在原登录会话恢复桌面。不安装系统服务，不创建辅助进程、计划任务或开机启动项，不修改系统登录策略。

当前是权限可行性实验，**尚未验证真实远程解锁**。默认 `serve` 不启用实验。普通用户模式继续只支持普通桌面。

## 试用步骤

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

CI 在 Windows runner 原生进行 Release / `--locked` 构建、测试、Clippy；在其他 GUI 检查后运行锁屏探测。探测是允许失败的实验步骤，其结果必须单独查看，不可用整个工作流成功掩盖探测失败。macOS 本机只验证 macOS 与共享模块。

参考：[线程模拟](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setthreadtoken)、[恢复线程身份](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-reverttoself)、[桌面绑定](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddesktop)、[SendInput 与 UIPI](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)。
