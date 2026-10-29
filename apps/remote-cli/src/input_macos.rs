use anyhow::{Context, Result, bail, ensure};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use remote_protocol::{KeyCode, KeyEvent, MouseAction, MouseButton, MouseEvent};
use remote_session::{InputController, InputSession};
use std::{
    collections::{BTreeSet, VecDeque},
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
    static kCGWindowNumber: Ref;
    static kCGWindowAlpha: Ref;
    fn CGRectMakeWithDictionaryRepresentation(dictionary: Ref, rect: *mut Rect) -> bool;
    fn CGEventCreateMouseEvent(source: Ref, kind: u32, point: Point, button: u32) -> Ref;
    fn CGEventCreateScrollWheelEvent(source: Ref, units: u32, count: u32, wheel1: i32, ...) -> Ref;
    fn CGEventSetLocation(event: Ref, point: Point);
    fn CGEventSetIntegerValueField(event: Ref, field: u32, value: i64);
    fn CGEventPost(tap: u32, event: Ref);
    fn CGEventCreateKeyboardEvent(source: Ref, virtual_key: u16, down: bool) -> Ref;
    fn CGEventSetFlags(event: Ref, flags: u64);
    fn CGEventSetType(event: Ref, kind: u32);
    fn CGEventPostToPid(pid: i32, event: Ref);
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
        "控制或移动共享窗口需要辅助功能权限：请在 系统设置 → 隐私与安全性 → 辅助功能 中允许当前终端或应用，再重启服务；普通只观看可加 --view-only；虚拟屏模式仍需要此权限"
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

fn ax_bounds(element: Ref) -> Option<Rect> {
    let position = attribute(element, c"AXPosition")?;
    let size = attribute(element, c"AXSize")?;
    let mut rect = Rect::default();
    // AX position and size values contain CGPoint and CGSize respectively.
    unsafe {
        (AXValueGetValue(position.0, 1, (&mut rect.origin as *mut Point).cast())
            && AXValueGetValue(size.0, 2, (&mut rect.size as *mut Size).cast()))
        .then_some(rect)
    }
}

fn ax_exposed(window_id: u32, pid: i32, wanted: Rect, point: Point) -> bool {
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
        let mut element = Owned(hit);
        let mut ancestors = Vec::new();
        // Embedded views can return a leaf whose reported bounds do not contain
        // the query point. Walk its containers, bounded and within the same app.
        for _ in 0..8 {
            AXUIElementSetMessagingTimeout(element.0, 0.25);
            let mut owner = 0;
            if AXUIElementGetPid(element.0, &mut owner) != 0 || owner != pid {
                break;
            }
            if let Some(window) = attribute(element.0, c"AXWindow") {
                return ax_bounds(window.0).is_some_and(|r| {
                    (r.origin.x - wanted.origin.x).abs() < 3.0
                        && (r.origin.y - wanted.origin.y).abs() < 3.0
                        && (r.size.width - wanted.size.width).abs() < 3.0
                        && (r.size.height - wanted.size.height).abs() < 3.0
                });
            }
            ancestors.push((owner, ax_bounds(element.0)));
            let Some(parent) = attribute(element.0, c"AXParent") else {
                break;
            };
            element = parent;
        }
        window_stack().is_some_and(|windows| {
            ancestors_belong_to_window(
                &windows,
                &StackedWindow {
                    id: window_id,
                    pid,
                    bounds: wanted,
                    alpha: 1.0,
                },
                &ancestors,
                point,
            )
        })
    }
}

struct StackedWindow {
    id: u32,
    pid: i32,
    bounds: Rect,
    alpha: f64,
}

fn top_window_at<'a>(
    windows: impl IntoIterator<Item = &'a StackedWindow>,
    point: Point,
) -> Option<(u32, i32)> {
    windows
        .into_iter()
        .find(|window| {
            let r = window.bounds;
            window.alpha > 0.0
                && point.x >= r.origin.x
                && point.y >= r.origin.y
                && point.x < r.origin.x + r.size.width
                && point.y < r.origin.y + r.size.height
        })
        .map(|window| (window.id, window.pid))
}

fn child_belongs_to_window(
    windows: &[StackedWindow],
    target: &StackedWindow,
    child: Rect,
    point: Point,
) -> bool {
    let wanted = target.bounds;
    child.size.width > 0.0
        && child.size.height > 0.0
        && point.x >= child.origin.x
        && point.y >= child.origin.y
        && point.x < child.origin.x + child.size.width
        && point.y < child.origin.y + child.size.height
        && child.origin.x >= wanted.origin.x - 3.0
        && child.origin.y >= wanted.origin.y - 3.0
        && child.origin.x + child.size.width <= wanted.origin.x + wanted.size.width + 3.0
        && child.origin.y + child.size.height <= wanted.origin.y + wanted.size.height + 3.0
        // Other-process overlays have already been resolved by AX hit testing.
        // Still distinguish overlapping windows belonging to the same app.
        && top_window_at(windows.iter().filter(|w| w.pid == target.pid), point)
            == Some((target.id, target.pid))
}

fn ancestors_belong_to_window(
    windows: &[StackedWindow],
    target: &StackedWindow,
    ancestors: &[(i32, Option<Rect>)],
    point: Point,
) -> bool {
    ancestors
        .iter()
        .take(8)
        .take_while(|(owner, _)| *owner == target.pid)
        .filter_map(|(_, bounds)| *bounds)
        .any(|bounds| child_belongs_to_window(windows, target, bounds, point))
}

fn window_stack() -> Option<Vec<StackedWindow>> {
    unsafe {
        // On-screen windows are returned front to back, across all layers.
        let list = Owned(CGWindowListCopyWindowInfo(1, 0));
        if list.0.is_null() {
            return None;
        }
        let mut windows = Vec::new();
        for index in 0..CFArrayGetCount(list.0) {
            let entry = CFArrayGetValueAtIndex(list.0, index);
            let mut id = 0i32;
            let mut owner = 0i32;
            let mut alpha = 1.0f64;
            let mut rect = Rect::default();
            let id_value = CFDictionaryGetValue(entry, kCGWindowNumber);
            let owner_value = CFDictionaryGetValue(entry, kCGWindowOwnerPID);
            let alpha_value = CFDictionaryGetValue(entry, kCGWindowAlpha);
            let rect_value = CFDictionaryGetValue(entry, kCGWindowBounds);
            // Incomplete metadata cannot prove that the target is exposed.
            if id_value.is_null()
                || owner_value.is_null()
                || alpha_value.is_null()
                || rect_value.is_null()
                || !CFNumberGetValue(id_value, 3, (&mut id as *mut i32).cast())
                || !CFNumberGetValue(owner_value, 3, (&mut owner as *mut i32).cast())
                || !CFNumberGetValue(alpha_value, 6, (&mut alpha as *mut f64).cast())
                || !CGRectMakeWithDictionaryRepresentation(rect_value, &mut rect)
            {
                return None;
            }
            windows.push(StackedWindow {
                id: id as u32,
                pid: owner,
                bounds: rect,
                alpha,
            });
        }
        Some(windows)
    }
}

fn stack_exposed(window: u32, pid: i32, point: Point) -> bool {
    window_stack().is_some_and(|windows| top_window_at(&windows, point) == Some((window, pid)))
}

fn exposed(window: u32, pid: i32, wanted: Rect, point: Point) -> bool {
    // Some apps return an AX child without usable AXWindow/geometry metadata.
    // The exact topmost CG window ID independently confirms those clicks.
    // Keep AX hit testing for transparent Dock/overlay regions.
    ax_exposed(window, pid, wanted, point) || stack_exposed(window, pid, point)
}

pub fn diagnose(window: u32, pid: i32, x: u16, y: u16) -> Result<()> {
    let rect = bounds(window, pid)?;
    let point = Point {
        x: rect.origin.x + f64::from(x) / 65535.0 * rect.size.width,
        y: rect.origin.y + f64::from(y) / 65535.0 * rect.size.height,
    };
    println!("{}", diagnostic_details(window, pid, rect, point));
    Ok(())
}

fn diagnostic_details(window: u32, pid: i32, rect: Rect, point: Point) -> String {
    use std::fmt::Write;
    let top = window_stack().map(|windows| top_window_at(&windows, point));
    let mut output = format!(
        "trusted={} target={window}/{pid} bounds={rect:?} point={point:?} CG-top={top:?}",
        unsafe { AXIsProcessTrusted() },
    );
    unsafe {
        let system = Owned(AXUIElementCreateSystemWide());
        if system.0.is_null() {
            output.push_str("; AX system unavailable");
            return output;
        }
        AXUIElementSetMessagingTimeout(system.0, 0.25);
        let mut hit = std::ptr::null();
        let status =
            AXUIElementCopyElementAtPosition(system.0, point.x as f32, point.y as f32, &mut hit);
        let _ = write!(output, "; AX-hit-status={status}");
        if status == 0 && !hit.is_null() {
            let mut element = Owned(hit);
            for depth in 0..4 {
                let mut owner = 0;
                AXUIElementGetPid(element.0, &mut owner);
                let mut p = Point::default();
                let mut s = Size::default();
                let position = attribute(element.0, c"AXPosition")
                    .is_some_and(|v| AXValueGetValue(v.0, 1, (&mut p as *mut Point).cast()));
                let size = attribute(element.0, c"AXSize")
                    .is_some_and(|v| AXValueGetValue(v.0, 2, (&mut s as *mut Size).cast()));
                let _ = write!(
                    output,
                    "; AX[{depth}] pid={owner} position={position}/{p:?} size={size}/{s:?} has-window={}",
                    attribute(element.0, c"AXWindow").is_some()
                );
                let Some(parent) = attribute(element.0, c"AXParent") else {
                    break;
                };
                element = parent;
            }
        }
    }
    output
}

fn prepare_target(window: u32, pid: i32, wanted: Rect, point: Point) -> Result<()> {
    // Keep hit testing on every click/scroll, but avoid expensive AX window
    // enumeration and activation when the target is already active and exposed.
    let active = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .is_some_and(|app| app.isActive());
    if active && exposed(window, pid, wanted, point) {
        return Ok(());
    }
    raise_window(pid, wanted)?;
    wait_until_exposed(window, pid, wanted, point)
}

fn wait_until_exposed(window: u32, pid: i32, wanted: Rect, point: Point) -> Result<()> {
    // Allow asynchronous application activation/window ordering to settle.
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        if exposed(window, pid, wanted, point) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!(
        "无法确认点击位置属于共享窗口，未发送操作。诊断：{}",
        diagnostic_details(window, pid, wanted, point)
    )
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

#[derive(Default)]
struct KeyboardState {
    held: BTreeSet<KeyCode>,
    caps_lock: bool,
}
fn keyboard_flags(state: &KeyboardState) -> u64 {
    let mut flags = if state.caps_lock { 1 << 16 } else { 0 };
    for key in &state.held {
        flags |= match key {
            KeyCode::LeftShift | KeyCode::RightShift => 1 << 17,
            KeyCode::LeftCtrl | KeyCode::RightCtrl => 1 << 18,
            KeyCode::LeftAlt | KeyCode::RightAlt => 1 << 19,
            KeyCode::LeftSuper | KeyCode::RightSuper => 1 << 20,
            _ => 0,
        };
    }
    flags
}
fn keyboard_window_focused(window: u32, pid: i32, wanted: Rect) -> bool {
    if !NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .is_some_and(|app| app.isActive())
    {
        return false;
    }
    let center = Point {
        x: wanted.origin.x + wanted.size.width / 2.0,
        y: wanted.origin.y + wanted.size.height / 2.0,
    };
    if !window_stack().is_some_and(|windows| {
        top_window_at(windows.iter().filter(|w| w.pid == pid), center) == Some((window, pid))
    }) {
        return false;
    }
    unsafe {
        let app = Owned(AXUIElementCreateApplication(pid));
        attribute(app.0, c"AXFocusedWindow")
            .and_then(|window| ax_bounds(window.0))
            .is_some_and(|rect| {
                (rect.origin.x - wanted.origin.x).abs() < 3.0
                    && (rect.origin.y - wanted.origin.y).abs() < 3.0
                    && (rect.size.width - wanted.size.width).abs() < 3.0
                    && (rect.size.height - wanted.size.height).abs() < 3.0
            })
    }
}

pub struct MacInput {
    window: u32,
    pid: i32,
    geometry: Mutex<VecDeque<Geometry>>,
    buttons: Mutex<Buttons>,
    keyboard: Mutex<KeyboardState>,
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
            keyboard: Mutex::new(KeyboardState::default()),
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
            CGEventSetFlags(
                event.0,
                keyboard_flags(&self.keyboard.lock().unwrap_or_else(|e| e.into_inner())),
            );
            CGEventPost(0, event.0);
        }
        Ok(())
    }
    fn release(&self) {
        self.release_mouse();
        self.release_keyboard();
    }
    fn release_mouse(&self) {
        let mut state = self.buttons.lock().unwrap_or_else(|e| e.into_inner());
        for (button, kind) in [2, 4, 26].into_iter().enumerate() {
            if state.held[button] {
                let _ = self.post(kind, state.point, button, state.click_count);
                state.held[button] = false;
            }
        }
    }
    fn post_key(&self, state: &mut KeyboardState, key: KeyCode, down: bool) -> Result<()> {
        let code = crate::keyboard::macos_key(key).context("Mac 不支持此按键")?;
        let event = Owned(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), code, down) });
        ensure!(!event.0.is_null(), "无法创建键盘事件");
        let repeated = down && state.held.contains(&key);
        if down {
            state.held.insert(key);
        } else {
            state.held.remove(&key);
        }
        if key == KeyCode::CapsLock && down {
            state.caps_lock = !state.caps_lock;
        }
        unsafe {
            CGEventSetFlags(event.0, keyboard_flags(state));
            CGEventSetIntegerValueField(event.0, 8, i64::from(repeated));
            if key.is_modifier() || key == KeyCode::CapsLock {
                CGEventSetType(event.0, 12);
            }
            // Target the selected app; never type into another foreground app.
            CGEventPostToPid(self.pid, event.0);
        }
        Ok(())
    }
    fn release_keyboard(&self) {
        let mut state = self.keyboard.lock().unwrap_or_else(|e| e.into_inner());
        let mut keys: Vec<_> = state.held.iter().copied().collect();
        keys.sort_by_key(|key| key.is_modifier());
        for key in keys {
            let _ = self.post_key(&mut state, key, false);
        }
    }
    fn apply_key(&self, event: KeyEvent) -> Result<()> {
        let mut state = self.keyboard.lock().unwrap_or_else(|e| e.into_inner());
        if !event.down {
            if state.held.contains(&event.key) {
                self.post_key(&mut state, event.key, false)?;
            }
            return Ok(());
        }
        ensure!(
            self.available.load(Ordering::Acquire) && !super::macos::is_locked(),
            "Mac 已锁屏或共享已结束"
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
            .context("画面已过期，请等待新画面")?;
        let current = bounds(self.window, self.pid)?;
        ensure!(
            (current.size.width - geometry.bounds.size.width).abs() < 3.0
                && (current.size.height - geometry.bounds.size.height).abs() < 3.0,
            "窗口尺寸已变化，请等待新画面"
        );
        if !keyboard_window_focused(self.window, self.pid, current) {
            raise_window(self.pid, current)?;
            let deadline = Instant::now() + Duration::from_millis(500);
            while !keyboard_window_focused(self.window, self.pid, current)
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            ensure!(
                keyboard_window_focused(self.window, self.pid, current),
                "无法确认共享窗口的键盘焦点"
            );
        }
        if state.held.contains(&event.key) && !event.key.repeatable() {
            return Ok(());
        }
        self.post_key(&mut state, event.key, true)
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
                    prepare_target(self.window, self.pid, current, point)?;
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
                prepare_target(self.window, self.pid, current, point)?;
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
                    CGEventSetFlags(
                        event.0,
                        keyboard_flags(&self.keyboard.lock().unwrap_or_else(|e| e.into_inner())),
                    );
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
    fn apply_key(&mut self, event: KeyEvent) -> Result<()> {
        self.0.apply_key(event)
    }
    fn release_mouse(&mut self) {
        self.0.release_mouse();
    }
    fn release_keyboard(&mut self) {
        self.0.release_keyboard();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_arrow_uses_containing_parent_when_ax_leaf_bounds_are_unrelated() {
        let target = StackedWindow {
            id: 25467,
            pid: 78641,
            bounds: Rect {
                origin: Point { x: 584.0, y: 25.0 },
                size: Size {
                    width: 246.0,
                    height: 465.0,
                },
            },
            alpha: 1.0,
        };
        let leaf = Rect {
            origin: Point { x: 659.0, y: 93.0 },
            size: Size {
                width: 33.0,
                height: 13.0,
            },
        };
        let parent = Rect {
            origin: Point { x: 584.0, y: 53.0 },
            size: Size {
                width: 247.0,
                height: 437.0,
            },
        };
        let ancestors = [
            (78641, Some(leaf)),
            (78641, Some(parent)),
            (63211, Some(parent)),
        ];
        let windows = [target];
        for point in [
            Point {
                x: 602.2168001831083,
                y: 66.96955825131609,
            },
            Point {
                x: 600.4112611581597,
                y: 65.52918287937743,
            },
        ] {
            assert!(!ancestors_belong_to_window(
                &windows,
                &windows[0],
                &ancestors[..1],
                point
            ));
            assert!(ancestors_belong_to_window(
                &windows,
                &windows[0],
                &ancestors,
                point
            ));
            assert!(!ancestors_belong_to_window(
                &windows,
                &windows[0],
                &[(78641, Some(leaf)), (63211, Some(parent))],
                point
            ));
            assert!(!ancestors_belong_to_window(
                &windows,
                &windows[0],
                &[(63211, Some(leaf)), (78641, Some(parent))],
                point
            ));
            assert!(ancestors_belong_to_window(
                &windows,
                &windows[0],
                &[(78641, None), (78641, Some(parent))],
                point
            ));
        }
        let overlap = StackedWindow {
            id: 99,
            ..window(99, 78641, 584.0, 1.0)
        };
        let overlap = StackedWindow {
            bounds: windows[0].bounds,
            ..overlap
        };
        let overlapped = [
            overlap,
            StackedWindow {
                ..window(25467, 78641, 584.0, 1.0)
            },
        ];
        assert!(!ancestors_belong_to_window(
            &overlapped,
            &windows[0],
            &ancestors,
            Point { x: 602.2, y: 66.9 }
        ));
    }

    #[test]
    fn ax_child_without_window_accepts_reported_tonghuashun_button() {
        let target = StackedWindow {
            id: 25467,
            pid: 78641,
            bounds: Rect {
                origin: Point { x: 584.0, y: 25.0 },
                size: Size {
                    width: 246.0,
                    height: 465.0,
                },
            },
            alpha: 1.0,
        };
        let child = Rect {
            origin: Point { x: 594.0, y: 399.0 },
            size: Size {
                width: 227.0,
                height: 31.0,
            },
        };
        let point = Point {
            x: 694.6634012359808,
            y: 410.8999771114672,
        };
        let overlay = StackedWindow {
            id: 24,
            pid: 1602,
            bounds: Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size {
                    width: 1920.0,
                    height: 1080.0,
                },
            },
            alpha: 1.0,
        };
        let windows = [overlay, target];
        assert_eq!(top_window_at(&windows, point), Some((24, 1602)));
        assert!(child_belongs_to_window(&windows, &windows[1], child, point));
        assert!(!child_belongs_to_window(
            &windows,
            &windows[1],
            child,
            Point { x: 600.0, y: 380.0 }
        ));
        let oversized = Rect {
            size: Size {
                width: 500.0,
                ..child.size
            },
            ..child
        };
        assert!(!child_belongs_to_window(
            &windows,
            &windows[1],
            oversized,
            point
        ));
        assert!(!child_belongs_to_window(
            &windows[..1],
            &windows[1],
            child,
            point
        ));
    }

    #[test]
    fn ax_child_does_not_authorize_an_overlapping_window_of_the_same_app() {
        let mut windows = [window(2, 10, 0.0, 1.0), window(1, 10, 0.0, 1.0)];
        let child = Rect {
            origin: Point { x: 10.0, y: 10.0 },
            size: Size {
                width: 20.0,
                height: 20.0,
            },
        };
        let point = Point { x: 20.0, y: 20.0 };
        assert!(!child_belongs_to_window(
            &windows,
            &windows[1],
            child,
            point
        ));
        windows[0].alpha = 0.0;
        assert!(child_belongs_to_window(&windows, &windows[1], child, point));
    }

    fn window(id: u32, pid: i32, x: f64, alpha: f64) -> StackedWindow {
        StackedWindow {
            id,
            pid,
            bounds: Rect {
                origin: Point { x, y: 0.0 },
                size: Size {
                    width: 100.0,
                    height: 100.0,
                },
            },
            alpha,
        }
    }

    #[test]
    fn stacking_checks_the_click_point_and_exact_window_not_just_pid() {
        let windows = [window(2, 10, 50.0, 1.0), window(1, 10, 0.0, 1.0)];
        assert_eq!(
            top_window_at(&windows, Point { x: 75.0, y: 50.0 }),
            Some((2, 10))
        );
        assert_eq!(
            top_window_at(&windows, Point { x: 25.0, y: 50.0 }),
            Some((1, 10))
        );
        assert_eq!(top_window_at(&windows, Point { x: 150.0, y: 50.0 }), None);
    }

    #[test]
    fn stacking_ignores_invisible_windows_but_preserves_visible_overlays() {
        let mut windows = [window(2, 20, 0.0, 0.0), window(1, 10, 0.0, 1.0)];
        let point = Point { x: 50.0, y: 50.0 };
        assert_eq!(top_window_at(&windows, point), Some((1, 10)));
        windows[0].alpha = 0.5;
        assert_eq!(top_window_at(&windows, point), Some((2, 20)));
        assert_eq!(top_window_at(&windows[0..0], point), None);
    }
}
