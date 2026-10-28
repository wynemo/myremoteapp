//! One-shot, manually supplied password delivery to Apple's own loginwindow.
//! No credential store, password logging, global password typing, or retries.
use crate::input_macos::{Owned, Ref, attribute, string};
use anyhow::{Context, Result, ensure};
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;
use remote_protocol::UnlockOutcome;
use remote_session::UnlockHost;
use std::{
    collections::VecDeque,
    ffi::{CStr, c_char, c_void},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(value: Ref) -> Ref;
    fn CFEqual(a: Ref, b: Ref) -> bool;
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: Ref) -> bool;
    fn CFStringGetLength(value: Ref) -> isize;
    fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: bool,
    ) -> Ref;
    fn CFArrayGetCount(value: Ref) -> isize;
    fn CFArrayGetValueAtIndex(value: Ref, index: isize) -> Ref;
    fn CFDictionaryGetValue(value: Ref, key: Ref) -> Ref;
    fn CFDictionaryCreate(
        allocator: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        key_callbacks: Ref,
        value_callbacks: Ref,
    ) -> Ref;
    fn CFNumberCreate(allocator: Ref, kind: i32, value: *const c_void) -> Ref;
    fn CFNumberGetValue(number: Ref, kind: i32, value: *mut c_void) -> bool;
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> Ref;
    fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
    fn CGEventPost(tap: u32, event: Ref);
}
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    fn NSUserName() -> Ref;
    fn NSFullUserName() -> Ref;
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementSetMessagingTimeout(element: Ref, seconds: f32) -> i32;
    fn AXUIElementSetAttributeValue(element: Ref, attribute: Ref, value: Ref) -> i32;
    fn AXUIElementIsAttributeSettable(element: Ref, attribute: Ref, result: *mut u8) -> i32;
    fn AXUIElementPerformAction(element: Ref, action: Ref) -> i32;
}
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecGuestAttributePid: Ref;
    fn SecCodeCopyGuestWithAttributes(
        host: Ref,
        attributes: Ref,
        flags: u32,
        code: *mut Ref,
    ) -> i32;
    fn SecRequirementCreateWithString(text: Ref, flags: u32, requirement: *mut Ref) -> i32;
    fn SecCodeCheckValidity(code: Ref, flags: u32, requirement: Ref) -> i32;
}
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionDeclareUserActivity(name: Ref, kind: u32, id: *mut u32) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}
unsafe extern "C" {
    fn getuid() -> u32;
    fn geteuid() -> u32;
}

fn text(value: Ref) -> Option<String> {
    unsafe {
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let length = CFStringGetLength(value);
        if !(0..=8192).contains(&length) {
            return None;
        }
        let mut bytes = vec![0u8; length as usize * 4 + 1];
        if !CFStringGetCString(
            value,
            bytes.as_mut_ptr().cast(),
            bytes.len() as isize,
            0x0800_0100,
        ) {
            return None;
        }
        CStr::from_bytes_until_nul(&bytes)
            .ok()?
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}
fn attr_text(element: Ref, name: &CStr) -> Option<String> {
    attribute(element, name).and_then(|value| text(value.0))
}
fn boolean(value: Ref) -> bool {
    unsafe {
        !value.is_null() && CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value)
    }
}
fn attr_bool(element: Ref, name: &CStr) -> bool {
    attribute(element, name).is_some_and(|v| boolean(v.0))
}
fn session_locked() -> Result<bool> {
    unsafe {
        let uid = getuid();
        ensure!(uid != 0 && uid == geteuid(), "unavailable");
        let session = Owned(CGSessionCopyCurrentDictionary());
        ensure!(!session.0.is_null(), "unavailable");
        let key = string(c"kCGSSessionUserIDKey");
        let number = CFDictionaryGetValue(session.0, key.0);
        let mut owner = 0u32;
        ensure!(
            !number.is_null()
                && CFNumberGetValue(number, 3, (&mut owner as *mut u32).cast())
                && owner == uid,
            "unavailable"
        );
        for name in [c"kCGSSessionOnConsoleKey", c"kCGSessionLoginDoneKey"] {
            let key = string(name);
            ensure!(
                boolean(CFDictionaryGetValue(session.0, key.0)),
                "unavailable"
            );
        }
        let key = string(c"CGSSessionScreenIsLocked");
        Ok(boolean(CFDictionaryGetValue(session.0, key.0)))
    }
}
fn verify_loginwindow(pid: i32) -> Result<Owned> {
    unsafe {
        let number = Owned(CFNumberCreate(
            std::ptr::null(),
            3,
            (&pid as *const i32).cast(),
        ));
        ensure!(!number.0.is_null(), "unavailable");
        let attributes = Owned(CFDictionaryCreate(
            std::ptr::null(),
            &kSecGuestAttributePid,
            &number.0,
            1,
            std::ptr::null(),
            std::ptr::null(),
        ));
        let mut code = std::ptr::null();
        ensure!(
            !attributes.0.is_null()
                && SecCodeCopyGuestWithAttributes(std::ptr::null(), attributes.0, 0, &mut code)
                    == 0
                && !code.is_null(),
            "unavailable"
        );
        let code = Owned(code);
        let rule = string(c"identifier \"com.apple.loginwindow\" and anchor apple");
        let mut requirement = std::ptr::null();
        ensure!(
            SecRequirementCreateWithString(rule.0, 0, &mut requirement) == 0
                && !requirement.is_null(),
            "unavailable"
        );
        let requirement = Owned(requirement);
        ensure!(
            SecCodeCheckValidity(code.0, 0, requirement.0) == 0,
            "unavailable"
        );
        Ok(code)
    }
}

struct Node {
    element: Owned,
    id: String,
    role: String,
    subrole: String,
    parent: Option<usize>,
}
struct Target {
    pid: i32,
    window: Owned,
    field: Option<Owned>,
    submit: Option<Owned>,
    collapsed: bool,
}
fn collect() -> Result<Target> {
    ensure!(
        session_locked()? && unsafe { AXIsProcessTrusted() },
        "unavailable"
    );
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(
        "com.apple.loginwindow",
    ));
    ensure!(apps.count() == 1, "unavailable");
    let app = apps.firstObject().context("unavailable")?;
    ensure!(
        app.executableURL()
            .and_then(|u| u.path())
            .is_some_and(|p| p.to_string()
                == "/System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow"),
        "unavailable"
    );
    let pid = app.processIdentifier();
    let _signed = verify_loginwindow(pid)?;
    let root = Owned(unsafe { AXUIElementCreateApplication(pid) });
    ensure!(!root.0.is_null(), "unavailable");
    unsafe {
        AXUIElementSetMessagingTimeout(root.0, 0.1);
    }
    let windows = attribute(root.0, c"AXWindows").context("unavailable")?;
    ensure!(
        unsafe { CFGetTypeID(windows.0) == CFArrayGetTypeID() },
        "unavailable"
    );
    let count = unsafe { CFArrayGetCount(windows.0) };
    ensure!((1..=16).contains(&count), "unavailable");
    let names = [unsafe { text(NSUserName()) }, unsafe {
        text(NSFullUserName())
    }];
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut candidates = Vec::new();
    for index in 0..count {
        let window = unsafe { CFArrayGetValueAtIndex(windows.0, index) };
        if attr_text(window, c"AXIdentifier").as_deref() != Some("login") {
            continue;
        }
        let mut nodes = Vec::new();
        walk(window, None, 0, deadline, &mut nodes)?;
        let fields: Vec<_> = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.id == "UserPasswordTextField")
            .map(|(i, _)| i)
            .collect();
        let labels: Vec<_> = nodes.iter().filter(|n| n.id == "FocusedUser").collect();
        ensure!(labels.len() <= 1, "unavailable");
        let own_label = if let Some(label) = labels.first() {
            ensure!(label.role == "AXStaticText", "unavailable");
            let label = attr_text(label.element.0, c"AXValue").context("unavailable")?;
            ensure!(
                names.iter().flatten().any(|name| name == &label),
                "unavailable"
            );
            true
        } else {
            false
        };
        if fields.is_empty() {
            if own_label {
                candidates.push(Target {
                    pid,
                    window: Owned(unsafe { CFRetain(window) }),
                    field: None,
                    submit: None,
                    collapsed: true,
                });
            }
            continue;
        }
        ensure!(fields.len() == 1, "unavailable");
        let field = &nodes[fields[0]];
        ensure!(
            field.role == "AXTextField"
                && field.subrole == "AXSecureTextField"
                && attr_bool(field.element.0, c"AXEnabled"),
            "unavailable"
        );
        let value = string(c"AXValue");
        let mut settable = 0u8;
        ensure!(
            unsafe { AXUIElementIsAttributeSettable(field.element.0, value.0, &mut settable) } == 0
                && settable != 0,
            "unavailable"
        );
        let buttons: Vec<_> = nodes
            .iter()
            .filter(|node| node.id == "LUIBUTTON_GO")
            .collect();
        ensure!(buttons.len() <= 1, "unavailable");
        let submit = if let Some(button) = buttons.first() {
            ensure!(
                button.role == "AXButton" && button.parent == field.parent,
                "unavailable"
            );
            attr_bool(button.element.0, c"AXEnabled")
                .then(|| Owned(unsafe { CFRetain(button.element.0) }))
        } else {
            None
        };
        candidates.push(Target {
            pid,
            window: Owned(unsafe { CFRetain(window) }),
            field: Some(Owned(unsafe { CFRetain(field.element.0) })),
            submit,
            collapsed: false,
        });
    }
    let fields: Vec<_> = candidates
        .iter()
        .enumerate()
        .filter(|(_, target)| target.field.is_some())
        .map(|(index, _)| index)
        .collect();
    if fields.len() == 1 {
        return Ok(candidates.remove(fields[0]));
    }
    ensure!(fields.is_empty() && candidates.len() == 1, "unavailable");
    Ok(candidates.remove(0))
}
fn walk(
    element: Ref,
    parent: Option<usize>,
    depth: usize,
    deadline: Instant,
    nodes: &mut Vec<Node>,
) -> Result<()> {
    ensure!(
        Instant::now() < deadline
            && depth <= 12
            && nodes.len() < 256
            && !nodes
                .iter()
                .any(|n| unsafe { CFEqual(n.element.0, element) }),
        "unavailable"
    );
    unsafe {
        AXUIElementSetMessagingTimeout(element, 0.1);
    }
    let index = nodes.len();
    nodes.push(Node {
        element: Owned(unsafe { CFRetain(element) }),
        id: attr_text(element, c"AXIdentifier").unwrap_or_default(),
        role: attr_text(element, c"AXRole").unwrap_or_default(),
        subrole: attr_text(element, c"AXSubrole").unwrap_or_default(),
        parent,
    });
    if let Some(children) = attribute(element, c"AXChildren") {
        ensure!(
            unsafe { CFGetTypeID(children.0) == CFArrayGetTypeID() },
            "unavailable"
        );
        let count = unsafe { CFArrayGetCount(children.0) };
        ensure!((0..=256).contains(&count), "unavailable");
        for i in 0..count {
            walk(
                unsafe { CFArrayGetValueAtIndex(children.0, i) },
                Some(index),
                depth + 1,
                deadline,
                nodes,
            )?;
        }
    }
    Ok(())
}
fn same_field(a: &Target, b: &Target) -> bool {
    a.pid == b.pid
        && unsafe { CFEqual(a.window.0, b.window.0) }
        && match (&a.field, &b.field) {
            (Some(a), Some(b)) => unsafe { CFEqual(a.0, b.0) },
            _ => false,
        }
}
struct Activity(u32);
impl Drop for Activity {
    fn drop(&mut self) {
        unsafe {
            IOPMAssertionRelease(self.0);
        }
    }
}
struct ClearField {
    field: Owned,
    pid: i32,
}
impl Drop for ClearField {
    fn drop(&mut self) {
        if session_locked().unwrap_or(false)
            && verify_loginwindow(self.pid).is_ok()
            && attr_text(self.field.0, c"AXIdentifier").as_deref() == Some("UserPasswordTextField")
            && attr_text(self.field.0, c"AXSubrole").as_deref() == Some("AXSecureTextField")
        {
            let key = string(c"AXValue");
            let empty = string(c"");
            unsafe {
                AXUIElementSetAttributeValue(self.field.0, key.0, empty.0);
            }
        }
    }
}
fn submit(password: &[u8], cancelled: &AtomicBool) -> Result<UnlockOutcome> {
    let active = || -> Result<()> {
        ensure!(!cancelled.load(Ordering::Acquire), "cancelled");
        Ok(())
    };
    active()?;
    let activity_name = string(c"MyRemoteApp manual unlock");
    let mut id = 0;
    ensure!(
        unsafe { IOPMAssertionDeclareUserActivity(activity_name.0, 1, &mut id) } == 0,
        "unavailable"
    );
    let _activity = Activity(id);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut revealed = false;
    let original = loop {
        ensure!(Instant::now() < deadline, "unavailable");
        active()?;
        let target = match collect() {
            Ok(target) => target,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if target.field.is_some() {
            break target;
        }
        if target.collapsed && !revealed {
            let next = collect()?;
            ensure!(
                next.pid == target.pid
                    && unsafe { CFEqual(next.window.0, target.window.0) }
                    && session_locked()?,
                "unavailable"
            );
            let down = Owned(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), 36, true) });
            let up = Owned(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), 36, false) });
            ensure!(!down.0.is_null() && !up.0.is_null(), "unavailable");
            unsafe {
                CGEventPost(0, down.0);
                CGEventPost(0, up.0);
            }
            revealed = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    active()?;
    let current = collect()?;
    ensure!(same_field(&original, &current), "unavailable");
    let field = current.field.as_ref().context("unavailable")?;
    let _clear = ClearField {
        field: Owned(unsafe { CFRetain(field.0) }),
        pid: current.pid,
    };
    let value = Owned(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            password.as_ptr(),
            password.len() as isize,
            0x0800_0100,
            false,
        )
    });
    let key = string(c"AXValue");
    active()?;
    ensure!(
        !value.0.is_null() && unsafe { AXUIElementSetAttributeValue(field.0, key.0, value.0) } == 0,
        "unavailable"
    );
    drop(value);
    let deadline = Instant::now() + Duration::from_secs(2);
    let ready = loop {
        active()?;
        let target = collect()?;
        ensure!(same_field(&original, &target), "unavailable");
        if target.submit.is_some() {
            break target;
        }
        ensure!(Instant::now() < deadline, "unavailable");
        std::thread::sleep(Duration::from_millis(50));
    };
    let final_target = collect()?;
    ensure!(
        same_field(&original, &final_target)
            && match (&ready.submit, &final_target.submit) {
                (Some(a), Some(b)) => unsafe { CFEqual(a.0, b.0) },
                _ => false,
            },
        "unavailable"
    );
    active()?;
    let action = string(c"AXPress");
    let status =
        unsafe { AXUIElementPerformAction(final_target.submit.as_ref().unwrap().0, action.0) };
    ensure!(status == 0 || status == -25204, "unavailable");
    let deadline = Instant::now() + Duration::from_secs(7);
    while Instant::now() < deadline {
        active()?;
        if matches!(session_locked(), Ok(false)) {
            return Ok(UnlockOutcome::Unlocked);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(UnlockOutcome::Rejected)
}

pub struct NativeUnlockHost {
    enabled: bool,
    ready: Arc<AtomicBool>,
    recovery_requested: Arc<AtomicBool>,
    busy: Mutex<()>,
    attempts: Mutex<VecDeque<Instant>>,
}
impl NativeUnlockHost {
    pub fn new(enabled: bool, ready: Arc<AtomicBool>, recovery_requested: Arc<AtomicBool>) -> Self {
        Self {
            enabled,
            ready,
            recovery_requested,
            busy: Mutex::new(()),
            attempts: Mutex::new(VecDeque::new()),
        }
    }
}
impl UnlockHost for NativeUnlockHost {
    fn status(&self) -> (bool, bool) {
        let locked = crate::macos::is_locked();
        if locked {
            self.ready.store(false, Ordering::Release);
            self.recovery_requested.store(true, Ordering::Release);
        }
        (
            locked,
            self.enabled && unsafe { AXIsProcessTrusted() } && session_locked().is_ok(),
        )
    }
    fn capture_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
            && !self.recovery_requested.load(Ordering::Acquire)
            && !crate::macos::is_locked()
    }
    fn unlock(&self, password: &[u8], cancelled: &AtomicBool) -> UnlockOutcome {
        if cancelled.load(Ordering::Acquire)
            || password.is_empty()
            || password.len() > 4096
            || password.contains(&0)
            || std::str::from_utf8(password).is_err()
        {
            return UnlockOutcome::Unavailable;
        }
        if !self.enabled || !unsafe { AXIsProcessTrusted() } {
            return UnlockOutcome::Unavailable;
        }
        match session_locked() {
            Ok(false) => return UnlockOutcome::Unlocked,
            Err(_) => return UnlockOutcome::Unavailable,
            _ => {}
        }
        self.ready.store(false, Ordering::Release);
        self.recovery_requested.store(true, Ordering::Release);
        let Ok(_guard) = self.busy.try_lock() else {
            return UnlockOutcome::Busy;
        };
        let mut attempts = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
        while attempts
            .front()
            .is_some_and(|t| t.elapsed() > Duration::from_secs(60))
        {
            attempts.pop_front();
        }
        if attempts.len() >= 3
            || attempts
                .back()
                .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
        {
            return UnlockOutcome::Busy;
        }
        attempts.push_back(Instant::now());
        drop(attempts);
        submit(password, cancelled).unwrap_or(UnlockOutcome::Unavailable)
    }
}
