use anyhow::{Context, Result, bail, ensure};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use remote_protocol::{MouseAction, MouseButton, MouseEvent};
use remote_session::{InputController, InputSession};
use std::{
    collections::VecDeque,
    ffi::{c_char, c_void},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) type Ref = *const c_void;
#[derive(Debug, Default, Copy, Clone)]
#[repr(C)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
#[derive(Debug, Default, Copy, Clone)]
#[repr(C)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}
#[derive(Debug, Default, Copy, Clone)]
#[repr(C)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(options: u32, window: u32) -> Ref;
    static kCGWindowBounds: Ref;
    static kCGWindowOwnerPID: Ref;
    fn CGRectMakeWithDictionaryRepresentation(dictionary: Ref, rect: *mut Rect) -> bool;
    fn CGEventCreateMouseEvent(source: Ref, kind: u32, point: Point, button: u32) -> Ref;
    fn CGEventCreateScrollWheelEvent(source: Ref, units: u32, count: u32, wheel1: i32, ...) -> Ref;
    fn CGEventSetLocation(event: Ref, point: Point);
    fn CGEventSetIntegerValueField(event: Ref, field: u32, value: i64);
    fn CGEventPost(tap: u32, event: Ref);
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Ref);
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
    fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
    fn CFNumberGetValue(number: Ref, kind: i32, value: *mut c_void) -> bool;
    fn CFStringCreateWithCString(allocator: Ref, string: *const c_char, encoding: u32) -> Ref;
    fn CFDictionaryCreate(
        allocator: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        key_callbacks: Ref,
        value_callbacks: Ref,
    ) -> Ref;
    static kCFBooleanTrue: Ref;
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: Ref) -> bool;
    static kAXTrustedCheckOptionPrompt: Ref;
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementCreateSystemWide() -> Ref;
    fn AXUIElementCopyElementAtPosition(element: Ref, x: f32, y: f32, result: *mut Ref) -> i32;
    fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
    fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    fn AXUIElementSetAttributeValue(element: Ref, attribute: Ref, value: Ref) -> i32;
    fn AXUIElementPerformAction(element: Ref, action: Ref) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, timeout: f32) -> i32;
    fn AXValueGetValue(value: Ref, kind: u32, output: *mut c_void) -> bool;
}

pub(crate) struct Owned(pub(crate) Ref);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}
pub(crate) fn string(value: &std::ffi::CStr) -> Owned {
    Owned(unsafe { CFStringCreateWithCString(std::ptr::null(), value.as_ptr(), 0x0800_0100) })
}
pub(crate) fn attribute(element: Ref, name: &std::ffi::CStr) -> Option<Owned> {
    let name = string(name);
    let mut result = std::ptr::null();
    (unsafe { AXUIElementCopyAttributeValue(element, name.0, &mut result) } == 0
        && !result.is_null())
    .then_some(Owned(result))
}

pub fn require_permission() -> Result<()> {
    if unsafe { AXIsProcessTrusted() } {
        return Ok(());
    }
    // Request the standard system prompt; granting access remains a local user action.
    unsafe {
        let options = Owned(CFDictionaryCreate(
            std::ptr::null(),
            &kAXTrustedCheckOptionPrompt,
            &kCFBooleanTrue,
            1,
            std::ptr::null(),
            std::ptr::null(),
        ));
        if !options.0.is_null() {
            AXIsProcessTrustedWithOptions(options.0);
        }
    }
    bail!(
        "鼠标控制需要辅助功能权限：请在 系统设置 → 隐私与安全性 → 辅助功能 中允许当前终端或应用，再重启服务；只观看可加 --view-only"
    )
}

fn bounds(window: u32, expected_pid: i32) -> Result<Rect> {
    unsafe {
        let list = Owned(CGWindowListCopyWindowInfo(8, window));
        ensure!(
            !list.0.is_null() && CFArrayGetCount(list.0) == 1,
            "共享窗口已关闭"
        );
        let entry = CFArrayGetValueAtIndex(list.0, 0);
        let owner = CFDictionaryGetValue(entry, kCGWindowOwnerPID);
        let mut pid = 0i32;
        ensure!(
            !owner.is_null()
                && CFNumberGetValue(owner, 3, (&mut pid as *mut i32).cast())
                && pid == expected_pid,
            "共享窗口已变更"
        );
        let dictionary = CFDictionaryGetValue(entry, kCGWindowBounds);
        let mut rect = Rect::default();
        ensure!(
            !dictionary.is_null() && CGRectMakeWithDictionaryRepresentation(dictionary, &mut rect),
            "无法读取窗口位置"
        );
        ensure!(
            rect.size.width > 0.0 && rect.size.height > 0.0,
            "共享窗口不可见"
        );
        Ok(rect)
    }
}

fn raise_window(pid: i32, wanted: Rect) -> Result<()> {
    unsafe {
        let app = Owned(AXUIElementCreateApplication(pid));
        ensure!(!app.0.is_null(), "目标 App 已退出");
        AXUIElementSetMessagingTimeout(app.0, 0.25);
        let windows = attribute(app.0, c"AXWindows").context("目标 App 没有提供可操作窗口")?;
        let mut matched = Vec::new();
        for index in 0..CFArrayGetCount(windows.0).min(128) {
            let window = CFArrayGetValueAtIndex(windows.0, index);
            AXUIElementSetMessagingTimeout(window, 0.25);
            let Some(position) = attribute(window, c"AXPosition") else {
                continue;
            };
            let Some(size) = attribute(window, c"AXSize") else {
                continue;
            };
            let mut p = Point::default();
            let mut s = Size::default();
            if AXValueGetValue(position.0, 1, (&mut p as *mut Point).cast())
                && AXValueGetValue(size.0, 2, (&mut s as *mut Size).cast())
                && (p.x - wanted.origin.x).abs() < 3.0
                && (p.y - wanted.origin.y).abs() < 3.0
                && (s.width - wanted.size.width).abs() < 3.0
                && (s.height - wanted.size.height).abs() < 3.0
            {
                matched.push(window);
            }
        }
        ensure!(
            matched.len() == 1,
            "无法唯一定位共享窗口，请移动目标窗口后重试"
        );
        let running = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .context("目标 App 已退出")?;
        running.unhide();
        let frontmost = string(c"AXFrontmost");
        AXUIElementSetAttributeValue(app.0, frontmost.0, kCFBooleanTrue);
        running.activateWithOptions(NSApplicationActivationOptions::empty());
        let main = string(c"AXMain");
        AXUIElementSetAttributeValue(matched[0], main.0, kCFBooleanTrue);
        let raise = string(c"AXRaise");
        ensure!(
            AXUIElementPerformAction(matched[0], raise.0) == 0,
            "无法激活共享窗口，请检查辅助功能权限"
        );
    }
    Ok(())
}

fn exposed(pid: i32, wanted: Rect, point: Point) -> bool {
    unsafe {
        let system = Owned(AXUIElementCreateSystemWide());
        if system.0.is_null() {
            return false;
        }
        AXUIElementSetMessagingTimeout(system.0, 0.25);
        let mut hit = std::ptr::null();
        if AXUIElementCopyElementAtPosition(system.0, point.x as f32, point.y as f32, &mut hit) != 0
            || hit.is_null()
        {
            return false;
        }
        let hit = Owned(hit);
        let mut owner = 0;
        if AXUIElementGetPid(hit.0, &mut owner) != 0 || owner != pid {
            return false;
        }
        let window = attribute(hit.0, c"AXWindow");
        let window = window.as_ref().map_or(hit.0, |window| window.0);
        let Some(position) = attribute(window, c"AXPosition") else {
            return false;
        };
        let Some(size) = attribute(window, c"AXSize") else {
            return false;
        };
        let mut p = Point::default();
        let mut s = Size::default();
        AXValueGetValue(position.0, 1, (&mut p as *mut Point).cast())
            && AXValueGetValue(size.0, 2, (&mut s as *mut Size).cast())
            && (p.x - wanted.origin.x).abs() < 3.0
            && (p.y - wanted.origin.y).abs() < 3.0
            && (s.width - wanted.size.width).abs() < 3.0
            && (s.height - wanted.size.height).abs() < 3.0
    }
}
fn wait_until_exposed(pid: i32, wanted: Rect, point: Point) -> Result<()> {
    // AX hit testing respects transparent Dock/overlay regions; bounding-box
    // window lists cannot distinguish those from actual overlapping controls.
    for _ in 0..15 {
        if exposed(pid, wanted, point) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("共享窗口被其他窗口遮挡，未发送点击；请在 Mac 将目标窗口置前后重试")
}

#[derive(Clone)]
struct Geometry {
    sequence: u64,
    bounds: Rect,
    content: Rect,
    width: u32,
    height: u32,
}
struct Buttons {
    held: [bool; 3],
    point: Point,
    last_click: Option<(MouseButton, Point, Instant, i64)>,
    click_count: i64,
}
impl Default for Buttons {
    fn default() -> Self {
        Self {
            held: [false; 3],
            point: Point::default(),
            last_click: None,
            click_count: 1,
        }
    }
}

pub struct MacInput {
    window: u32,
    pid: i32,
    geometry: Mutex<VecDeque<Geometry>>,
    buttons: Mutex<Buttons>,
    leased: AtomicBool,
    available: AtomicBool,
}
impl MacInput {
    pub fn new(window: u32, pid: i32) -> Arc<Self> {
        Arc::new(Self {
            window,
            pid,
            geometry: Mutex::new(VecDeque::new()),
            buttons: Mutex::new(Buttons::default()),
            leased: AtomicBool::new(false),
            available: AtomicBool::new(true),
        })
    }
    pub fn record(&self, sequence: u64, width: u32, height: u32, content: Rect) {
        if let Ok(bounds) = bounds(self.window, self.pid) {
            let mut history = self.geometry.lock().unwrap_or_else(|e| e.into_inner());
            history.push_back(Geometry {
                sequence,
                bounds,
                content,
                width,
                height,
            });
            while history.len() > 128 {
                history.pop_front();
            }
        }
    }
    pub fn stop(&self) {
        self.available.store(false, Ordering::Release);
        self.release();
    }
    fn post(&self, kind: u32, point: Point, button: usize, clicks: i64) -> Result<()> {
        let event =
            Owned(unsafe { CGEventCreateMouseEvent(std::ptr::null(), kind, point, button as u32) });
        ensure!(!event.0.is_null(), "无法创建鼠标事件");
        unsafe {
            CGEventSetIntegerValueField(event.0, 1, clicks);
            CGEventPost(0, event.0);
        }
        Ok(())
    }
    fn release(&self) {
        let mut state = self.buttons.lock().unwrap_or_else(|e| e.into_inner());
        for (button, kind) in [2, 4, 26].into_iter().enumerate() {
            if state.held[button] {
                let _ = self.post(kind, state.point, button, state.click_count);
                state.held[button] = false;
            }
        }
    }
    fn apply(&self, event: MouseEvent) -> Result<()> {
        let mut state = self.buttons.lock().unwrap_or_else(|e| e.into_inner());
        // Releases remain valid even if the displayed frame/window geometry expired.
        if let MouseAction::Button {
            button,
            down: false,
        } = event.kind
        {
            let index = button_index(button);
            if state.held[index] {
                self.post([2, 4, 26][index], state.point, index, state.click_count)?;
                state.held[index] = false;
            }
            return Ok(());
        }
        ensure!(
            self.available.load(Ordering::Acquire) && !super::macos::is_locked(),
            "Mac 已锁屏或共享已结束，请解锁后重启共享"
        );
        ensure!(
            unsafe { AXIsProcessTrusted() },
            "辅助功能权限已失效，请在 Mac 重新授权"
        );
        let geometry = self
            .geometry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|g| g.sequence == event.frame_sequence)
            .cloned()
            .context("画面已过期，请等待最新画面后重试")?;
        let current = bounds(self.window, self.pid)?;
        ensure!(
            (current.size.width - geometry.bounds.size.width).abs() < 3.0
                && (current.size.height - geometry.bounds.size.height).abs() < 3.0,
            "窗口尺寸已变化，请等待最新画面后重试"
        );
        let px = f64::from(event.x) / 65535.0 * f64::from(geometry.width);
        let py = f64::from(event.y) / 65535.0 * f64::from(geometry.height);
        let c = geometry.content;
        ensure!(
            c.size.width > 0.0
                && c.size.height > 0.0
                && px >= c.origin.x
                && py >= c.origin.y
                && px < c.origin.x + c.size.width
                && py < c.origin.y + c.size.height,
            "请在共享窗口画面内操作"
        );
        let point = Point {
            x: current.origin.x + (px - c.origin.x) / c.size.width * current.size.width,
            y: current.origin.y + (py - c.origin.y) / c.size.height * current.size.height,
        };
        match event.kind {
            MouseAction::Move => {
                let (kind, button) = if state.held[0] {
                    (6, 0)
                } else if state.held[1] {
                    (7, 1)
                } else if state.held[2] {
                    (27, 2)
                } else {
                    (5, 0)
                };
                self.post(kind, point, button, state.click_count)?;
            }
            MouseAction::Button { button, down: true } => {
                let index = button_index(button);
                if !state.held[index] {
                    raise_window(self.pid, current)?;
                    wait_until_exposed(self.pid, current, point)?;
                    let count = state
                        .last_click
                        .filter(|(b, p, t, _)| {
                            *b == button
                                && t.elapsed() < Duration::from_millis(500)
                                && (p.x - point.x).abs() < 4.0
                                && (p.y - point.y).abs() < 4.0
                        })
                        .map_or(1, |(_, _, _, n)| (n + 1).min(3));
                    state.click_count = count;
                    state.last_click = Some((button, point, Instant::now(), count));
                    self.post([1, 3, 25][index], point, index, count)?;
                    state.held[index] = true;
                }
            }
            MouseAction::Scroll { dx, dy } => {
                raise_window(self.pid, current)?;
                wait_until_exposed(self.pid, current, point)?;
                let event = Owned(unsafe {
                    CGEventCreateScrollWheelEvent(
                        std::ptr::null(),
                        1,
                        2,
                        i32::from(dy.clamp(-50, 50)),
                        i32::from(dx.clamp(-50, 50)),
                    )
                });
                ensure!(!event.0.is_null(), "无法创建滚轮事件");
                unsafe {
                    CGEventSetLocation(event.0, point);
                    CGEventPost(0, event.0);
                }
            }
            MouseAction::Button { down: false, .. } => unreachable!(),
        }
        state.point = point;
        Ok(())
    }
}
fn button_index(button: MouseButton) -> usize {
    match button {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
    }
}

// Keep the Arc in the controller handle so cancellation drops an owning session.
pub struct Controller(pub Arc<MacInput>);
impl InputController for Controller {
    fn acquire(&self) -> Result<Box<dyn InputSession>> {
        ensure!(self.0.available.load(Ordering::Acquire), "共享已结束");
        ensure!(
            !self.0.leased.swap(true, Ordering::AcqRel),
            "已有客户端正在控制此窗口"
        );
        Ok(Box::new(Session(self.0.clone())))
    }
}
struct Session(Arc<MacInput>);
impl InputSession for Session {
    fn apply(&mut self, event: MouseEvent) -> Result<()> {
        self.0.apply(event)
    }
    fn release_all(&mut self) {
        self.0.release();
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.0.release();
        self.0.leased.store(false, Ordering::Release);
    }
}
