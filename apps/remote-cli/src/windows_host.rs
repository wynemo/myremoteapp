//! Interactive Windows desktop host. All GDI objects stay on the capture thread.
use crate::desktop_geometry::{DesktopFrames, DesktopRect};
use anyhow::{Context, Result, ensure};
use remote_protocol::{
    KeyCode, KeyEvent, MouseAction, MouseButton, MouseEvent, PreviewFrame, WindowInfo,
};
use remote_session::{CaptureEvent, InputController, InputSession, PreviewDemand, PreviewSource};
use std::{
    collections::BTreeSet,
    mem::{size_of, size_of_val},
    ptr::null_mut,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::watch;
use windows_sys::Win32::{
    Graphics::Gdi::*,
    System::{StationsAndDesktops::*, Threading::GetCurrentThreadId},
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
        Input::KeyboardAndMouse::*,
        WindowsAndMessaging::{
            GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
            SM_YVIRTUALSCREEN, WHEEL_DELTA,
        },
    },
};

fn desktop_rect() -> Result<DesktopRect> {
    let rect = unsafe {
        DesktopRect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    };
    ensure!(
        rect.width > 0 && rect.height > 0,
        "无法读取 Windows 桌面尺寸"
    );
    Ok(rect)
}

fn physical_desktop_rect() -> Result<DesktopRect> {
    // Capture and input must both measure physical pixels on their own threads.
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    ensure!(!previous.is_null(), "无法读取物理桌面坐标");
    let result = desktop_rect();
    unsafe {
        SetThreadDpiAwarenessContext(previous);
    }
    result
}

// Bind only the calling worker to the current input desktop. Network/runtime
// threads never retain a secure-desktop binding across operations.
struct DesktopBinding {
    desktop: HDESK,
    previous: HDESK,
    name: String,
}
impl DesktopBinding {
    fn open() -> Result<Self> {
        unsafe {
            let previous = GetThreadDesktop(GetCurrentThreadId());
            ensure!(!previous.is_null(), "无法保存原线程桌面");
            let desktop = OpenInputDesktop(
                0,
                0,
                DESKTOP_READOBJECTS
                    | DESKTOP_WRITEOBJECTS
                    | DESKTOP_SWITCHDESKTOP
                    | DESKTOP_JOURNALPLAYBACK,
            );
            ensure!(
                !desktop.is_null(),
                "无法访问 Windows 当前桌面：{}",
                std::io::Error::last_os_error()
            );
            let mut binding = Self {
                desktop,
                previous,
                name: String::new(),
            };
            binding.name = desktop_name(desktop)?;
            ensure!(
                binding.name.eq_ignore_ascii_case("Default")
                    || (crate::windows_privilege::effective_system()
                        && binding.name.eq_ignore_ascii_case("Winlogon")),
                "当前进程无法访问 Windows 安全桌面，请以管理员身份启动共享"
            );
            ensure!(
                SetThreadDesktop(desktop) != 0,
                "无法切换 Windows 采集/输入线程桌面：{}",
                std::io::Error::last_os_error()
            );
            Ok(binding)
        }
    }
}
impl Drop for DesktopBinding {
    fn drop(&mut self) {
        unsafe {
            if SetThreadDesktop(self.previous) == 0 {
                std::process::abort();
            }
            CloseDesktop(self.desktop);
        }
    }
}
fn desktop_name(desktop: HDESK) -> Result<String> {
    let mut name = [0u16; 256];
    let mut needed = 0;
    ensure!(
        unsafe {
            GetUserObjectInformationW(
                desktop,
                UOI_NAME,
                name.as_mut_ptr().cast(),
                size_of_val(&name) as u32,
                &mut needed,
            )
        } != 0,
        "无法读取 Windows 桌面身份"
    );
    Ok(String::from_utf16_lossy(
        &name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())],
    ))
}
fn current_desktop_name() -> Option<String> {
    unsafe {
        let desktop = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if desktop.is_null() {
            return None;
        }
        let name = desktop_name(desktop).ok();
        CloseDesktop(desktop);
        name
    }
}
fn desktop_available() -> bool {
    current_desktop_name().is_some_and(|name| {
        name.eq_ignore_ascii_case("Default")
            || (crate::windows_privilege::effective_system()
                && name.eq_ignore_ascii_case("Winlogon"))
    })
}

pub struct CaptureSession {
    pub source: PreviewSource,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn start(view_only: bool, experimental_lock_screen: bool) -> Result<CaptureSession> {
    let stop = Arc::new(AtomicBool::new(false));
    let input = Arc::new(Controller(Arc::new(Mutex::new(InputState::default()))));
    let (sender, frames) = watch::channel(None);
    let (init_tx, init_rx) = std::sync::mpsc::sync_channel(1);
    let demand = PreviewDemand::default();
    let capture_demand = demand.clone();
    let stopped = stop.clone();
    let controller = input.clone();
    let worker = std::thread::Builder::new()
        .name("windows-desktop".into())
        .spawn(move || {
            // This changes only the worker, not the client's native window policy.
            let dpi =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            let initial = (|| -> Result<_> {
                let identity = if experimental_lock_screen {
                    Some(crate::windows_privilege::ThreadIdentity::enter()?)
                } else {
                    None
                };
                ensure!(!dpi.is_null(), "无法启用 Windows 物理像素坐标");
                ensure!(
                    desktop_available(),
                    "请在已登录且解锁的 Windows 桌面启动服务"
                );
                let _desktop = DesktopBinding::open()?;
                Ok((desktop_rect()?, identity))
            })();
            let (rect, _identity) = match initial {
                Ok(value) => value,
                Err(error) => {
                    let _ = init_tx.send(Err(format!("{error:#}")));
                    return;
                }
            };
            let (width, height) = rect.frame_size();
            if init_tx.send(Ok((width, height))).is_err() {
                return;
            }
            let mut sequence = 0;
            let mut sharing = false;
            while !stopped.load(Ordering::Acquire) {
                if !capture_demand.is_active() {
                    if sharing {
                        controller.stop();
                        sender.send_replace(None);
                        sharing = false;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                sharing = true;
                let tick = Instant::now();
                let result = (|| -> Result<_> {
                    ensure!(
                        desktop_available(),
                        "Windows 当前桌面不可访问；请以管理员身份启动共享以访问锁屏"
                    );
                    let binding = DesktopBinding::open()?;
                    let rect = desktop_rect()?;
                    let frame = capture(rect, sequence)?;
                    ensure!(
                        current_desktop_name().as_deref() == Some(&binding.name),
                        "Windows 桌面已切换，请恢复桌面后重新连接"
                    );
                    Ok((rect, frame, binding.name.clone()))
                })();
                match result {
                    Ok((rect, frame, desktop)) => {
                        controller.record(sequence, rect, desktop);
                        sender.send_replace(Some(CaptureEvent::Frame(Arc::new(frame))));
                        sequence += 1;
                    }
                    Err(error) => {
                        controller.stop();
                        sender.send_replace(Some(CaptureEvent::Stopped(format!("{error:#}"))));
                    }
                }
                std::thread::sleep(Duration::from_millis(100).saturating_sub(tick.elapsed()));
            }
            controller.stop();
            sender.send_replace(Some(CaptureEvent::Stopped("Windows 服务已停止".into())));
        })
        .context("无法启动 Windows 桌面采集线程")?;
    let initialized = init_rx.recv().context("Windows 桌面采集线程提前退出")?;
    let (width, height) = match initialized {
        Ok(size) => size,
        Err(message) => {
            let _ = worker.join();
            anyhow::bail!(message);
        }
    };
    println!(
        "正在共享 Windows 整个桌面（包含所有显示器）：{width}×{height}，有客户端观看时采集，最多 10 FPS。"
    );
    Ok(CaptureSession {
        source: PreviewSource {
            window: WindowInfo {
                id: 0,
                app: "Windows".into(),
                title: "整个桌面（所有显示器）".into(),
                width,
                height,
            },
            frames,
            demand,
            input: if view_only {
                None
            } else {
                Some(Arc::new(ThreadController {
                    controller: input,
                    experimental_lock_screen,
                }))
            },
        },
        stop,
        worker: Some(worker),
    })
}

// Only the worker below owns the real InputSession. Network threads exchange
// bounded commands and never acquire SYSTEM impersonation or desktop handles.
struct ThreadController {
    controller: Arc<Controller>,
    experimental_lock_screen: bool,
}
enum InputCommand {
    Mouse(MouseEvent),
    Key(KeyEvent),
    ReleaseMouse,
    ReleaseKeyboard,
    ReleaseAll,
}
type InputRequest = (InputCommand, std::sync::mpsc::SyncSender<Result<()>>);
struct ThreadSession {
    sender: Option<std::sync::mpsc::SyncSender<InputRequest>>,
    worker: Option<JoinHandle<()>>,
}
impl InputController for ThreadController {
    fn acquire(&self) -> Result<Box<dyn InputSession>> {
        let controller = self.controller.clone();
        let experimental_lock_screen = self.experimental_lock_screen;
        let (sender, receiver) = std::sync::mpsc::sync_channel::<InputRequest>(8);
        let (initialized, ready) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("windows-input".into())
            .spawn(move || {
                let setup = (|| -> Result<_> {
                    let identity = if experimental_lock_screen {
                        Some(crate::windows_privilege::ThreadIdentity::enter()?)
                    } else {
                        None
                    };
                    let session = controller.acquire()?;
                    Ok((identity, session))
                })();
                let (_identity, mut session) = match setup {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = initialized.send(Err(format!("{error:#}")));
                        return;
                    }
                };
                if initialized.send(Ok(())).is_err() {
                    return;
                }
                while let Ok((command, reply)) = receiver.recv() {
                    let result = match command {
                        InputCommand::Mouse(event) => session.apply(event),
                        InputCommand::Key(event) => session.apply_key(event),
                        InputCommand::ReleaseMouse => {
                            session.release_mouse();
                            Ok(())
                        }
                        InputCommand::ReleaseKeyboard => {
                            session.release_keyboard();
                            Ok(())
                        }
                        InputCommand::ReleaseAll => {
                            session.release_all();
                            Ok(())
                        }
                    };
                    let _ = reply.send(result);
                }
                // session releases all held input before the impersonation ends.
                drop(session);
            })
            .context("无法启动 Windows 输入线程")?;
        let session = ThreadSession {
            sender: Some(sender),
            worker: Some(worker),
        };
        ready
            .recv()
            .context("Windows 输入线程提前退出")?
            .map_err(anyhow::Error::msg)?;
        Ok(Box::new(session))
    }
}
impl ThreadSession {
    fn request(&self, command: InputCommand) -> Result<()> {
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .context("Windows 输入已结束")?
            .send((command, reply))
            .map_err(|_| anyhow::anyhow!("Windows 输入线程已停止"))?;
        result.recv().context("Windows 输入线程未响应")?
    }
}
impl InputSession for ThreadSession {
    fn apply(&mut self, event: MouseEvent) -> Result<()> {
        self.request(InputCommand::Mouse(event))
    }
    fn apply_key(&mut self, event: KeyEvent) -> Result<()> {
        self.request(InputCommand::Key(event))
    }
    fn release_mouse(&mut self) {
        let _ = self.request(InputCommand::ReleaseMouse);
    }
    fn release_keyboard(&mut self) {
        let _ = self.request(InputCommand::ReleaseKeyboard);
    }
    fn release_all(&mut self) {
        let _ = self.request(InputCommand::ReleaseAll);
    }
}
impl Drop for ThreadSession {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Surface {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_null() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            if !self.screen.is_null() {
                ReleaseDC(null_mut(), self.screen);
            }
        }
    }
}
fn capture(rect: DesktopRect, sequence: u64) -> Result<PreviewFrame> {
    let (width, height) = rect.frame_size();
    remote_protocol::validate_frame_size(width, height)?;
    let mut surface = Surface {
        screen: null_mut(),
        memory: null_mut(),
        bitmap: null_mut(),
        previous: null_mut(),
    };
    let mut pixels = null_mut();
    unsafe {
        surface.screen = GetDC(null_mut());
        ensure!(!surface.screen.is_null(), "无法打开 Windows 桌面设备");
        surface.memory = CreateCompatibleDC(surface.screen);
        ensure!(!surface.memory.is_null(), "无法创建采集设备");
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            ..std::mem::zeroed()
        };
        surface.bitmap = CreateDIBSection(
            surface.screen,
            &info,
            DIB_RGB_COLORS,
            &mut pixels,
            null_mut(),
            0,
        );
        ensure!(
            !surface.bitmap.is_null() && !pixels.is_null(),
            "无法分配采集位图"
        );
        surface.previous = SelectObject(surface.memory, surface.bitmap);
        if surface.previous as isize == -1 {
            surface.previous = null_mut();
        }
        ensure!(!surface.previous.is_null(), "无法选中采集位图");
        SetStretchBltMode(surface.memory, HALFTONE);
        SetBrushOrgEx(surface.memory, 0, 0, null_mut());
        ensure!(
            StretchBlt(
                surface.memory,
                0,
                0,
                width as i32,
                height as i32,
                surface.screen,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                // CAPTUREBLT can hide/show the software cursor on every grab.
                SRCCOPY
            ) != 0,
            "Windows 桌面采集失败"
        );
        ensure!(GdiFlush() != 0, "Windows 桌面像素同步失败");
    }
    // The selected top-down 32-bit DIB stays alive throughout JPEG encoding.
    let bytes = unsafe {
        std::slice::from_raw_parts(pixels.cast::<u8>(), width as usize * height as usize * 4)
    };
    let mut jpeg = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut jpeg, 95);
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4);
    encoder.encode(
        bytes,
        width as u16,
        height as u16,
        jpeg_encoder::ColorType::Bgra,
    )?;
    ensure!(
        jpeg.len() <= remote_protocol::MAX_FRAME_BYTES,
        "桌面 JPEG 超出传输上限"
    );
    Ok(PreviewFrame {
        width,
        height,
        sequence,
        jpeg,
    })
}

#[derive(Default)]
struct InputState {
    active: bool,
    available: bool,
    held: [bool; 3],
    keys: BTreeSet<KeyCode>,
    frames: DesktopFrames,
    desktop: String,
}
#[derive(Clone)]
struct Controller(Arc<Mutex<InputState>>);
impl Controller {
    fn record(&self, sequence: u64, rect: DesktopRect, desktop: String) {
        let mut state = self.0.lock().unwrap();
        if state.desktop != desktop {
            release_all_inputs(&mut state);
            state.frames.clear();
            state.desktop = desktop;
        }
        if state.frames.layout_changed(rect) {
            release_all_inputs(&mut state);
        }
        if !state.available {
            release_all_inputs(&mut state);
        }
        state.available = true;
        state.frames.record(sequence, rect);
    }
    fn stop(&self) {
        let mut state = self.0.lock().unwrap();
        state.available = false;
        state.frames.clear();
        release_all_inputs(&mut state);
    }
}
impl InputController for Controller {
    fn acquire(&self) -> Result<Box<dyn InputSession>> {
        let mut state = self.0.lock().unwrap();
        ensure!(
            state.available && desktop_available(),
            "Windows 桌面当前不可控制"
        );
        ensure!(!state.active, "另一个客户端正在控制键鼠");
        release_all_inputs(&mut state);
        ensure!(
            !state.held.iter().any(|held| *held) && state.keys.is_empty(),
            "无法释放之前的键鼠按键，请恢复 Windows 桌面后重试"
        );
        state.active = true;
        Ok(Box::new(ControlSession(self.clone())))
    }
}
struct ControlSession(Controller);
impl InputSession for ControlSession {
    fn apply(&mut self, event: MouseEvent) -> Result<()> {
        let mut state = self.0.0.lock().unwrap();
        let result = (|| -> Result<()> {
            let binding = DesktopBinding::open()?;
            ensure!(
                binding.name == state.desktop,
                "Windows 桌面已切换，请等待新画面"
            );
            ensure!(state.available && desktop_available(), "Windows 桌面已暂停");
            let rect = state
                .frames
                .get(event.frame_sequence)
                .context("画面已过期，请等待新画面")?;
            ensure!(
                rect == physical_desktop_rect()?,
                "显示器布局已变化，请等待新画面"
            );
            send_mouse(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                0,
                i32::from(event.x),
                i32::from(event.y),
            )?;
            match event.kind {
                MouseAction::Move => {}
                MouseAction::Button { button, down } => {
                    let (index, flag) = button_flag(button, down);
                    if state.held[index] != down {
                        send_mouse(flag, 0, 0, 0)?;
                        state.held[index] = down;
                    }
                }
                MouseAction::Scroll { dx, dy } => {
                    if dy != 0 {
                        send_mouse(
                            MOUSEEVENTF_WHEEL,
                            (i32::from(dy) * WHEEL_DELTA as i32) as u32,
                            0,
                            0,
                        )?;
                    }
                    if dx != 0 {
                        send_mouse(
                            MOUSEEVENTF_HWHEEL,
                            (i32::from(dx) * WHEEL_DELTA as i32) as u32,
                            0,
                            0,
                        )?;
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            release_all_inputs(&mut state);
        }
        result
    }
    fn apply_key(&mut self, event: KeyEvent) -> Result<()> {
        let mut state = self.0.0.lock().unwrap();
        let result = (|| -> Result<()> {
            let binding = DesktopBinding::open()?;
            ensure!(
                binding.name == state.desktop,
                "Windows 桌面已切换，请等待新画面"
            );
            if event.down {
                ensure!(state.available && desktop_available(), "Windows 桌面已暂停");
                let rect = state
                    .frames
                    .get(event.frame_sequence)
                    .context("画面已过期，请等待新画面")?;
                ensure!(
                    rect == physical_desktop_rect()?,
                    "显示器布局已变化，请等待新画面"
                );
                if state.keys.contains(&event.key) && !event.key.repeatable() {
                    return Ok(());
                }
                send_key(event.key, true)?;
                state.keys.insert(event.key);
            } else if state.keys.contains(&event.key) {
                send_key(event.key, false)?;
                state.keys.remove(&event.key);
            }
            Ok(())
        })();
        if result.is_err() {
            release_all_inputs(&mut state);
        }
        result
    }
    fn release_mouse(&mut self) {
        release_mouse_buttons(&mut self.0.0.lock().unwrap());
    }
    fn release_keyboard(&mut self) {
        release_keys(&mut self.0.0.lock().unwrap());
    }
    fn release_all(&mut self) {
        release_all_inputs(&mut self.0.0.lock().unwrap());
    }
}
impl Drop for ControlSession {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap();
        release_all_inputs(&mut state);
        state.active = false;
    }
}
fn button_flag(button: MouseButton, down: bool) -> (usize, u32) {
    match (button, down) {
        (MouseButton::Left, true) => (0, MOUSEEVENTF_LEFTDOWN),
        (MouseButton::Left, false) => (0, MOUSEEVENTF_LEFTUP),
        (MouseButton::Right, true) => (1, MOUSEEVENTF_RIGHTDOWN),
        (MouseButton::Right, false) => (1, MOUSEEVENTF_RIGHTUP),
        (MouseButton::Middle, true) => (2, MOUSEEVENTF_MIDDLEDOWN),
        (MouseButton::Middle, false) => (2, MOUSEEVENTF_MIDDLEUP),
    }
}
fn release_all_inputs(state: &mut InputState) {
    release_mouse_buttons(state);
    release_keys(state);
}
fn release_keys(state: &mut InputState) {
    let mut keys: Vec<_> = state.keys.iter().copied().collect();
    keys.sort_by_key(|key| key.is_modifier());
    for key in keys {
        if send_key(key, false).is_ok() {
            state.keys.remove(&key);
        }
    }
}
fn release_mouse_buttons(state: &mut InputState) {
    for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
        let (index, flag) = button_flag(button, false);
        if state.held[index] && send_mouse(flag, 0, 0, 0).is_ok() {
            state.held[index] = false;
        }
    }
}
fn send_mouse(flags: u32, data: u32, x: i32, y: i32) -> Result<()> {
    let _desktop = DesktopBinding::open()?;
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: x,
                dy: y,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    ensure!(
        unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } == 1,
        "Windows 拒绝鼠标输入，请检查运行权限和系统策略"
    );
    Ok(())
}

fn send_key(key: KeyCode, down: bool) -> Result<()> {
    let _desktop = DesktopBinding::open()?;
    let (virtual_key, extended) = crate::keyboard::windows_key(key);
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: 0,
                dwFlags: if extended { KEYEVENTF_EXTENDEDKEY } else { 0 }
                    | if down { 0 } else { KEYEVENTF_KEYUP },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    ensure!(
        unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } == 1,
        "Windows 拒绝键盘输入；线程模拟不保证能通过 UIPI，请查看单进程验证说明"
    );
    Ok(())
}

/// Native proof-of-concept. Does not read or submit credentials. A successful
/// SendInput call is only API acceptance, not proof that LogonUI handled it.
pub fn probe(lock: bool, ci_unlock: bool) -> Result<()> {
    let password = if ci_unlock {
        ensure!(
            std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
                && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted"),
            "真实解锁探测仅允许在一次性 GitHub-hosted runner 运行"
        );
        use std::io::Read;
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        std::io::stdin().take(129).read_to_end(&mut bytes)?;
        ensure!(
            (4..=128).contains(&bytes.len())
                && bytes
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'!'),
            "无效的临时测试凭据"
        );
        Some(bytes)
    } else {
        None
    };
    let process_id = std::process::id();
    println!("single-process probe: pid={process_id}; lock={lock}");
    std::thread::spawn(move || -> Result<()> {
        let _identity = crate::windows_privilege::ThreadIdentity::enter().context("stage=thread_identity")?;
        println!("stage=thread_identity effective_system={}", crate::windows_privilege::effective_system());
        ensure!(crate::windows_privilege::effective_system(), "探测需要以管理员身份运行");
        let capture_probe = || -> Result<()> {
            let binding = DesktopBinding::open().context("stage=desktop_binding")?;
            println!("stage=desktop_binding desktop={}", binding.name);
            let rect = physical_desktop_rect()?;
            let frame = capture(rect, 0).context("stage=capture")?;
            // Decode only in memory. Never upload a login-screen screenshot.
            let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(&frame.jpeg));
            let pixels = decoder.decode()?;
            let low = pixels.iter().min().copied().unwrap_or(0);
            let high = pixels.iter().max().copied().unwrap_or(0);
            println!("stage=capture dimensions={}x{} pixel_range={low}..{high}", frame.width, frame.height);
            ensure!(high.saturating_sub(low) > 2, "采集结果接近纯色，不能作为锁屏画面可用的证据");
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT {
                    wVk: VK_F24, wScan: 0, dwFlags: KEYEVENTF_KEYUP, time: 0, dwExtraInfo: 0,
                } },
            };
            let impersonated = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
            println!("stage=input_api identity=thread key=F24 action=up accepted={impersonated}");
            let _process_identity = crate::windows_privilege::ProcessIdentity::enter()?;
            let accepted = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
            println!("stage=input_api identity=process key=F24 action=up accepted={accepted}; actual_unlock_verified=false");
            ensure!(accepted == 1 || impersonated == 1, "SendInput 在两种身份下都拒绝输入；单进程当前桌面控制探测失败");
            Ok(())
        };
        capture_probe().context("baseline desktop probe failed")?;
        if lock {
            let requested = {
                let _process_identity = crate::windows_privilege::ProcessIdentity::enter()?;
                unsafe { windows_sys::Win32::System::Shutdown::LockWorkStation() }
            };
            ensure!(requested != 0,
                "stage=lock_request failed: {}", std::io::Error::last_os_error());
            let deadline = Instant::now() + Duration::from_secs(15);
            while !current_desktop_name().is_some_and(|name| name.eq_ignore_ascii_case("Winlogon")) {
                ensure!(Instant::now() < deadline, "stage=lock_wait: 未能访问 Winlogon 桌面，不能验证锁屏采集或输入");
                std::thread::sleep(Duration::from_millis(100));
            }
            // Let LogonUI paint before sampling. This is not an unlock attempt.
            std::thread::sleep(Duration::from_secs(2));
            capture_probe().context("locked desktop probe failed")?;
        }
        ensure!(std::process::id() == process_id, "探测进程发生变化");
        if let Some(password) = password {
            verify_ci_unlock(&password)?;
            println!("probe complete; actual_unlock_verified=true; fixture_credential_submitted_once=true");
        } else {
            println!("probe complete; no login credential was submitted; actual_unlock_verified=false");
        }
        Ok(())
    }).join().map_err(|_| anyhow::anyhow!("Windows 探测线程异常退出"))?
}

// Exercises the actual dedicated input worker and frame checks. Only the CI
// fixture supplies a newly generated password over stdin; it is never logged.
fn verify_ci_unlock(password: &[u8]) -> Result<()> {
    ensure!(
        current_desktop_name().as_deref() == Some("Winlogon"),
        "解锁验收要求当前确实处于 Winlogon 桌面"
    );
    let controller = Arc::new(Controller(Arc::new(Mutex::new(InputState::default()))));
    let rect = physical_desktop_rect()?;
    controller.record(1, rect, "Winlogon".into());
    let mut input = ThreadController {
        controller,
        experimental_lock_screen: true,
    }
    .acquire()?;
    let mut key = |code, down| {
        input.apply_key(KeyEvent {
            frame_sequence: 1,
            key: code,
            down,
        })
    };
    let mut tap = |code| -> Result<()> {
        key(code, true)?;
        key(code, false)?;
        Ok(())
    };
    // Dismiss the clock/lock curtain; no credential has been typed yet.
    tap(KeyCode::Enter)?;
    std::thread::sleep(Duration::from_secs(2));
    drop(tap);
    key(KeyCode::LeftCtrl, true)?;
    key(KeyCode::A, true)?;
    key(KeyCode::A, false)?;
    key(KeyCode::LeftCtrl, false)?;
    key(KeyCode::Backspace, true)?;
    key(KeyCode::Backspace, false)?;
    if unsafe { GetKeyState(VK_CAPITAL as i32) } & 1 != 0 {
        key(KeyCode::CapsLock, true)?;
        key(KeyCode::CapsLock, false)?;
    }
    for byte in password {
        let (code, shift) = match byte {
            b'a' => (KeyCode::A, false),
            b'A' => (KeyCode::A, true),
            b'b' => (KeyCode::B, false),
            b'B' => (KeyCode::B, true),
            b'c' => (KeyCode::C, false),
            b'C' => (KeyCode::C, true),
            b'd' => (KeyCode::D, false),
            b'D' => (KeyCode::D, true),
            b'e' => (KeyCode::E, false),
            b'E' => (KeyCode::E, true),
            b'f' => (KeyCode::F, false),
            b'F' => (KeyCode::F, true),
            b'0' => (KeyCode::Key0, false),
            b'1' => (KeyCode::Key1, false),
            b'2' => (KeyCode::Key2, false),
            b'3' => (KeyCode::Key3, false),
            b'4' => (KeyCode::Key4, false),
            b'5' => (KeyCode::Key5, false),
            b'6' => (KeyCode::Key6, false),
            b'7' => (KeyCode::Key7, false),
            b'8' => (KeyCode::Key8, false),
            b'9' => (KeyCode::Key9, false),
            b'!' => (KeyCode::Key1, true),
            _ => anyhow::bail!("临时测试凭据包含不支持的字符"),
        };
        if shift {
            key(KeyCode::LeftShift, true)?;
        }
        key(code, true)?;
        key(code, false)?;
        if shift {
            key(KeyCode::LeftShift, false)?;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    // Exactly one submission; a failure does not retry the fixture password.
    key(KeyCode::Enter, true)?;
    key(KeyCode::Enter, false)?;
    input.release_all();
    drop(input);
    let deadline = Instant::now() + Duration::from_secs(20);
    while current_desktop_name().as_deref() != Some("Default") {
        ensure!(
            Instant::now() < deadline,
            "actual_unlock_verified=false：临时密码只提交了一次，登录界面未解锁；API 接受不等于输入生效"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("stage=actual_unlock desktop=Default; verification=one_fixture_password_submission");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_transition_invalidates_same_size_old_frames() {
        let controller = Controller(Arc::new(Mutex::new(InputState::default())));
        let rect = DesktopRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        controller.record(1, rect, "Default".into());
        controller.record(2, rect, "Winlogon".into());
        let state = controller.0.lock().unwrap();
        assert_eq!(state.frames.get(1), None);
        assert_eq!(state.frames.get(2), Some(rect));
        drop(state);
        controller.record(3, rect, "Default".into());
        let state = controller.0.lock().unwrap();
        assert_eq!(state.frames.get(2), None);
        assert_eq!(state.frames.get(3), Some(rect));
    }
}
