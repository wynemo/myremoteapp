//! Interactive Windows desktop host. All GDI objects stay on the capture thread.
use crate::desktop_geometry::{DesktopFrames, DesktopRect};
use anyhow::{Context, Result, ensure};
use remote_protocol::{MouseAction, MouseButton, MouseEvent, PreviewFrame, WindowInfo};
use remote_session::{CaptureEvent, InputController, InputSession, PreviewDemand, PreviewSource};
use std::{
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
    System::StationsAndDesktops::*,
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

// Never read or inject into a lock screen or UAC secure desktop. Opening the
// input desktop also fails when the interactive session is inaccessible.
fn desktop_available() -> bool {
    unsafe {
        let desktop = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if desktop.is_null() {
            return false;
        }
        let mut name = [0u16; 256];
        let mut needed = 0;
        let ok = GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            size_of_val(&name) as u32,
            &mut needed,
        );
        CloseDesktop(desktop);
        ok != 0
            && String::from_utf16_lossy(
                &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())],
            )
            .eq_ignore_ascii_case("Default")
    }
}

pub struct CaptureSession {
    pub source: PreviewSource,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    input: Arc<Controller>,
}
impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.input.stop();
    }
}

pub fn start(view_only: bool, experimental_lock_screen: bool) -> Result<CaptureSession> {
    ensure!(
        !experimental_lock_screen || view_only,
        "锁屏采集实验仅支持 --view-only"
    );
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
                ensure!(!dpi.is_null(), "无法启用 Windows 物理像素坐标");
                ensure!(
                    experimental_lock_screen || desktop_available(),
                    "请在已登录且解锁的 Windows 桌面启动服务"
                );
                desktop_rect()
            })();
            let rect = match initial {
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
            let mut last_desktop_available = None;
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
                let available = desktop_available();
                if experimental_lock_screen && last_desktop_available != Some(available) {
                    eprintln!("锁屏采集实验：Default 输入桌面可访问={available}；继续尝试 GDI 截图。");
                    last_desktop_available = Some(available);
                }
                let result = (|| -> Result<_> {
                    ensure!(
                        experimental_lock_screen || available,
                        "Windows 已锁屏或正在显示安全桌面；请在本机恢复桌面后重新连接"
                    );
                    let rect = desktop_rect()?;
                    let frame = capture(rect, sequence).with_context(|| {
                        format!("GDI 截图失败（Default 输入桌面可访问={available}，锁屏采集实验={experimental_lock_screen}）")
                    })?;
                    ensure!(
                        experimental_lock_screen || desktop_available(),
                        "Windows 桌面已切换，请恢复桌面后重新连接"
                    );
                    Ok((rect, frame))
                })();
                match result {
                    Ok((rect, frame)) => {
                        controller.record(sequence, rect);
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
            input: if view_only { None } else { Some(input.clone()) },
        },
        stop,
        worker: Some(worker),
        input,
    })
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
            "Windows 桌面采集失败：StretchBlt 返回 0；系统错误={}",
            std::io::Error::last_os_error()
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
    frames: DesktopFrames,
}
#[derive(Clone)]
struct Controller(Arc<Mutex<InputState>>);
impl Controller {
    fn record(&self, sequence: u64, rect: DesktopRect) {
        let mut state = self.0.lock().unwrap();
        if state.frames.layout_changed(rect) {
            release_buttons(&mut state);
        }
        if !state.available {
            release_buttons(&mut state);
        }
        state.available = true;
        state.frames.record(sequence, rect);
    }
    fn stop(&self) {
        let mut state = self.0.lock().unwrap();
        state.available = false;
        state.frames.clear();
        release_buttons(&mut state);
    }
}
impl InputController for Controller {
    fn acquire(&self) -> Result<Box<dyn InputSession>> {
        let mut state = self.0.lock().unwrap();
        ensure!(
            state.available && desktop_available(),
            "Windows 桌面当前不可控制"
        );
        ensure!(!state.active, "另一个客户端正在控制鼠标");
        release_buttons(&mut state);
        ensure!(
            !state.held.iter().any(|held| *held),
            "无法释放之前的鼠标按键，请恢复 Windows 桌面后重试"
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
            ensure!(state.available && desktop_available(), "Windows 桌面已暂停");
            let rect = state
                .frames
                .get(event.frame_sequence)
                .context("画面已过期，请等待新画面")?;
            // Physical coordinates on this Tokio worker must match capture's.
            let previous =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            ensure!(!previous.is_null(), "无法读取物理桌面坐标");
            let current = desktop_rect();
            unsafe {
                SetThreadDpiAwarenessContext(previous);
            }
            ensure!(rect == current?, "显示器布局已变化，请等待新画面");
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
            release_buttons(&mut state);
        }
        result
    }
    fn release_all(&mut self) {
        release_buttons(&mut self.0.0.lock().unwrap());
    }
}
impl Drop for ControlSession {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap();
        release_buttons(&mut state);
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
fn release_buttons(state: &mut InputState) {
    for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
        let (index, flag) = button_flag(button, false);
        if state.held[index] && send_mouse(flag, 0, 0, 0).is_ok() {
            state.held[index] = false;
        }
    }
}
fn send_mouse(flags: u32, data: u32, x: i32, y: i32) -> Result<()> {
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
        "Windows 拒绝鼠标输入（不能控制权限更高的程序或安全桌面）"
    );
    Ok(())
}
