//! Ordered native keyboard transitions and host key mappings.
use remote_protocol::{KeyCode, KeyEvent, Message};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

pub fn from_native(key: minifb::Key) -> Option<KeyCode> {
    Some(match key {
        minifb::Key::Key0 => KeyCode::Key0,
        minifb::Key::Key1 => KeyCode::Key1,
        minifb::Key::Key2 => KeyCode::Key2,
        minifb::Key::Key3 => KeyCode::Key3,
        minifb::Key::Key4 => KeyCode::Key4,
        minifb::Key::Key5 => KeyCode::Key5,
        minifb::Key::Key6 => KeyCode::Key6,
        minifb::Key::Key7 => KeyCode::Key7,
        minifb::Key::Key8 => KeyCode::Key8,
        minifb::Key::Key9 => KeyCode::Key9,
        minifb::Key::A => KeyCode::A,
        minifb::Key::B => KeyCode::B,
        minifb::Key::C => KeyCode::C,
        minifb::Key::D => KeyCode::D,
        minifb::Key::E => KeyCode::E,
        minifb::Key::F => KeyCode::F,
        minifb::Key::G => KeyCode::G,
        minifb::Key::H => KeyCode::H,
        minifb::Key::I => KeyCode::I,
        minifb::Key::J => KeyCode::J,
        minifb::Key::K => KeyCode::K,
        minifb::Key::L => KeyCode::L,
        minifb::Key::M => KeyCode::M,
        minifb::Key::N => KeyCode::N,
        minifb::Key::O => KeyCode::O,
        minifb::Key::P => KeyCode::P,
        minifb::Key::Q => KeyCode::Q,
        minifb::Key::R => KeyCode::R,
        minifb::Key::S => KeyCode::S,
        minifb::Key::T => KeyCode::T,
        minifb::Key::U => KeyCode::U,
        minifb::Key::V => KeyCode::V,
        minifb::Key::W => KeyCode::W,
        minifb::Key::X => KeyCode::X,
        minifb::Key::Y => KeyCode::Y,
        minifb::Key::Z => KeyCode::Z,
        minifb::Key::F1 => KeyCode::F1,
        minifb::Key::F2 => KeyCode::F2,
        minifb::Key::F3 => KeyCode::F3,
        minifb::Key::F4 => KeyCode::F4,
        minifb::Key::F5 => KeyCode::F5,
        minifb::Key::F6 => KeyCode::F6,
        minifb::Key::F7 => KeyCode::F7,
        minifb::Key::F8 => KeyCode::F8,
        minifb::Key::F9 => KeyCode::F9,
        minifb::Key::F10 => KeyCode::F10,
        minifb::Key::F11 => KeyCode::F11,
        minifb::Key::F12 => KeyCode::F12,
        minifb::Key::F13 => KeyCode::F13,
        minifb::Key::F14 => KeyCode::F14,
        minifb::Key::F15 => KeyCode::F15,
        minifb::Key::Down => KeyCode::Down,
        minifb::Key::Left => KeyCode::Left,
        minifb::Key::Right => KeyCode::Right,
        minifb::Key::Up => KeyCode::Up,
        minifb::Key::Apostrophe => KeyCode::Apostrophe,
        minifb::Key::Backquote => KeyCode::Backquote,
        minifb::Key::Backslash => KeyCode::Backslash,
        minifb::Key::Comma => KeyCode::Comma,
        minifb::Key::Equal => KeyCode::Equal,
        minifb::Key::LeftBracket => KeyCode::LeftBracket,
        minifb::Key::Minus => KeyCode::Minus,
        minifb::Key::Period => KeyCode::Period,
        minifb::Key::RightBracket => KeyCode::RightBracket,
        minifb::Key::Semicolon => KeyCode::Semicolon,
        minifb::Key::Slash => KeyCode::Slash,
        minifb::Key::Backspace => KeyCode::Backspace,
        minifb::Key::Delete => KeyCode::Delete,
        minifb::Key::End => KeyCode::End,
        minifb::Key::Enter => KeyCode::Enter,
        minifb::Key::Escape => KeyCode::Escape,
        minifb::Key::Home => KeyCode::Home,
        minifb::Key::Insert => KeyCode::Insert,
        minifb::Key::Menu => KeyCode::Menu,
        minifb::Key::PageDown => KeyCode::PageDown,
        minifb::Key::PageUp => KeyCode::PageUp,
        minifb::Key::Pause => KeyCode::Pause,
        minifb::Key::Space => KeyCode::Space,
        minifb::Key::Tab => KeyCode::Tab,
        minifb::Key::NumLock => KeyCode::NumLock,
        minifb::Key::CapsLock => KeyCode::CapsLock,
        minifb::Key::ScrollLock => KeyCode::ScrollLock,
        minifb::Key::LeftShift => KeyCode::LeftShift,
        minifb::Key::RightShift => KeyCode::RightShift,
        minifb::Key::LeftCtrl => KeyCode::LeftCtrl,
        minifb::Key::RightCtrl => KeyCode::RightCtrl,
        minifb::Key::NumPad0 => KeyCode::NumPad0,
        minifb::Key::NumPad1 => KeyCode::NumPad1,
        minifb::Key::NumPad2 => KeyCode::NumPad2,
        minifb::Key::NumPad3 => KeyCode::NumPad3,
        minifb::Key::NumPad4 => KeyCode::NumPad4,
        minifb::Key::NumPad5 => KeyCode::NumPad5,
        minifb::Key::NumPad6 => KeyCode::NumPad6,
        minifb::Key::NumPad7 => KeyCode::NumPad7,
        minifb::Key::NumPad8 => KeyCode::NumPad8,
        minifb::Key::NumPad9 => KeyCode::NumPad9,
        minifb::Key::NumPadDot => KeyCode::NumPadDot,
        minifb::Key::NumPadSlash => KeyCode::NumPadSlash,
        minifb::Key::NumPadAsterisk => KeyCode::NumPadAsterisk,
        minifb::Key::NumPadMinus => KeyCode::NumPadMinus,
        minifb::Key::NumPadPlus => KeyCode::NumPadPlus,
        minifb::Key::NumPadEnter => KeyCode::NumPadEnter,
        minifb::Key::LeftAlt => KeyCode::LeftAlt,
        minifb::Key::RightAlt => KeyCode::RightAlt,
        minifb::Key::LeftSuper => KeyCode::LeftSuper,
        minifb::Key::RightSuper => KeyCode::RightSuper,
        minifb::Key::Unknown | minifb::Key::Count => return None,
    })
}

#[cfg(any(target_os = "windows", test))]
pub fn windows_key(key: KeyCode) -> (u16, bool) {
    match key {
        KeyCode::Key0 => (0x30, false),
        KeyCode::Key1 => (0x31, false),
        KeyCode::Key2 => (0x32, false),
        KeyCode::Key3 => (0x33, false),
        KeyCode::Key4 => (0x34, false),
        KeyCode::Key5 => (0x35, false),
        KeyCode::Key6 => (0x36, false),
        KeyCode::Key7 => (0x37, false),
        KeyCode::Key8 => (0x38, false),
        KeyCode::Key9 => (0x39, false),
        KeyCode::A => (0x41, false),
        KeyCode::B => (0x42, false),
        KeyCode::C => (0x43, false),
        KeyCode::D => (0x44, false),
        KeyCode::E => (0x45, false),
        KeyCode::F => (0x46, false),
        KeyCode::G => (0x47, false),
        KeyCode::H => (0x48, false),
        KeyCode::I => (0x49, false),
        KeyCode::J => (0x4a, false),
        KeyCode::K => (0x4b, false),
        KeyCode::L => (0x4c, false),
        KeyCode::M => (0x4d, false),
        KeyCode::N => (0x4e, false),
        KeyCode::O => (0x4f, false),
        KeyCode::P => (0x50, false),
        KeyCode::Q => (0x51, false),
        KeyCode::R => (0x52, false),
        KeyCode::S => (0x53, false),
        KeyCode::T => (0x54, false),
        KeyCode::U => (0x55, false),
        KeyCode::V => (0x56, false),
        KeyCode::W => (0x57, false),
        KeyCode::X => (0x58, false),
        KeyCode::Y => (0x59, false),
        KeyCode::Z => (0x5a, false),
        KeyCode::F1 => (0x70, false),
        KeyCode::F2 => (0x71, false),
        KeyCode::F3 => (0x72, false),
        KeyCode::F4 => (0x73, false),
        KeyCode::F5 => (0x74, false),
        KeyCode::F6 => (0x75, false),
        KeyCode::F7 => (0x76, false),
        KeyCode::F8 => (0x77, false),
        KeyCode::F9 => (0x78, false),
        KeyCode::F10 => (0x79, false),
        KeyCode::F11 => (0x7a, false),
        KeyCode::F12 => (0x7b, false),
        KeyCode::F13 => (0x7c, false),
        KeyCode::F14 => (0x7d, false),
        KeyCode::F15 => (0x7e, false),
        KeyCode::Down => (0x28, true),
        KeyCode::Left => (0x25, true),
        KeyCode::Right => (0x27, true),
        KeyCode::Up => (0x26, true),
        KeyCode::Apostrophe => (0xde, false),
        KeyCode::Backquote => (0xc0, false),
        KeyCode::Backslash => (0xdc, false),
        KeyCode::Comma => (0xbc, false),
        KeyCode::Equal => (0xbb, false),
        KeyCode::LeftBracket => (0xdb, false),
        KeyCode::Minus => (0xbd, false),
        KeyCode::Period => (0xbe, false),
        KeyCode::RightBracket => (0xdd, false),
        KeyCode::Semicolon => (0xba, false),
        KeyCode::Slash => (0xbf, false),
        KeyCode::Backspace => (0x08, false),
        KeyCode::Delete => (0x2e, true),
        KeyCode::End => (0x23, true),
        KeyCode::Enter => (0x0d, false),
        KeyCode::Escape => (0x1b, false),
        KeyCode::Home => (0x24, true),
        KeyCode::Insert => (0x2d, true),
        KeyCode::Menu => (0x5d, true),
        KeyCode::PageDown => (0x22, true),
        KeyCode::PageUp => (0x21, true),
        KeyCode::Pause => (0x13, false),
        KeyCode::Space => (0x20, false),
        KeyCode::Tab => (0x09, false),
        KeyCode::NumLock => (0x90, true),
        KeyCode::CapsLock => (0x14, false),
        KeyCode::ScrollLock => (0x91, false),
        KeyCode::LeftShift => (0xa0, false),
        KeyCode::RightShift => (0xa1, false),
        KeyCode::LeftCtrl => (0xa2, false),
        KeyCode::RightCtrl => (0xa3, true),
        KeyCode::NumPad0 => (0x60, false),
        KeyCode::NumPad1 => (0x61, false),
        KeyCode::NumPad2 => (0x62, false),
        KeyCode::NumPad3 => (0x63, false),
        KeyCode::NumPad4 => (0x64, false),
        KeyCode::NumPad5 => (0x65, false),
        KeyCode::NumPad6 => (0x66, false),
        KeyCode::NumPad7 => (0x67, false),
        KeyCode::NumPad8 => (0x68, false),
        KeyCode::NumPad9 => (0x69, false),
        KeyCode::NumPadDot => (0x6e, false),
        KeyCode::NumPadSlash => (0x6f, true),
        KeyCode::NumPadAsterisk => (0x6a, false),
        KeyCode::NumPadMinus => (0x6d, false),
        KeyCode::NumPadPlus => (0x6b, false),
        KeyCode::NumPadEnter => (0x0d, true),
        KeyCode::LeftAlt => (0xa4, false),
        KeyCode::RightAlt => (0xa5, true),
        KeyCode::LeftSuper => (0x5b, true),
        KeyCode::RightSuper => (0x5c, true),
    }
}

#[cfg(target_os = "macos")]
pub fn macos_key(key: KeyCode) -> Option<u16> {
    match key {
        KeyCode::Key0 => Some(0x1d),
        KeyCode::Key1 => Some(0x12),
        KeyCode::Key2 => Some(0x13),
        KeyCode::Key3 => Some(0x14),
        KeyCode::Key4 => Some(0x15),
        KeyCode::Key5 => Some(0x17),
        KeyCode::Key6 => Some(0x16),
        KeyCode::Key7 => Some(0x1a),
        KeyCode::Key8 => Some(0x1c),
        KeyCode::Key9 => Some(0x19),
        KeyCode::A => Some(0x00),
        KeyCode::B => Some(0x0b),
        KeyCode::C => Some(0x08),
        KeyCode::D => Some(0x02),
        KeyCode::E => Some(0x0e),
        KeyCode::F => Some(0x03),
        KeyCode::G => Some(0x05),
        KeyCode::H => Some(0x04),
        KeyCode::I => Some(0x22),
        KeyCode::J => Some(0x26),
        KeyCode::K => Some(0x28),
        KeyCode::L => Some(0x25),
        KeyCode::M => Some(0x2e),
        KeyCode::N => Some(0x2d),
        KeyCode::O => Some(0x1f),
        KeyCode::P => Some(0x23),
        KeyCode::Q => Some(0x0c),
        KeyCode::R => Some(0x0f),
        KeyCode::S => Some(0x01),
        KeyCode::T => Some(0x11),
        KeyCode::U => Some(0x20),
        KeyCode::V => Some(0x09),
        KeyCode::W => Some(0x0d),
        KeyCode::X => Some(0x07),
        KeyCode::Y => Some(0x10),
        KeyCode::Z => Some(0x06),
        KeyCode::F1 => Some(0x7a),
        KeyCode::F2 => Some(0x78),
        KeyCode::F3 => Some(0x63),
        KeyCode::F4 => Some(0x76),
        KeyCode::F5 => Some(0x60),
        KeyCode::F6 => Some(0x61),
        KeyCode::F7 => Some(0x62),
        KeyCode::F8 => Some(0x64),
        KeyCode::F9 => Some(0x65),
        KeyCode::F10 => Some(0x6d),
        KeyCode::F11 => Some(0x67),
        KeyCode::F12 => Some(0x6f),
        KeyCode::F13 => Some(0x69),
        KeyCode::F14 => Some(0x6b),
        KeyCode::F15 => Some(0x71),
        KeyCode::Down => Some(0x7d),
        KeyCode::Left => Some(0x7b),
        KeyCode::Right => Some(0x7c),
        KeyCode::Up => Some(0x7e),
        KeyCode::Apostrophe => Some(0x27),
        KeyCode::Backquote => Some(0x32),
        KeyCode::Backslash => Some(0x2a),
        KeyCode::Comma => Some(0x2b),
        KeyCode::Equal => Some(0x18),
        KeyCode::LeftBracket => Some(0x21),
        KeyCode::Minus => Some(0x1b),
        KeyCode::Period => Some(0x2f),
        KeyCode::RightBracket => Some(0x1e),
        KeyCode::Semicolon => Some(0x29),
        KeyCode::Slash => Some(0x2c),
        KeyCode::Backspace => Some(0x33),
        KeyCode::Delete => Some(0x75),
        KeyCode::End => Some(0x77),
        KeyCode::Enter => Some(0x24),
        KeyCode::Escape => Some(0x35),
        KeyCode::Home => Some(0x73),
        KeyCode::Insert => Some(0x72),
        KeyCode::Menu => None,
        KeyCode::PageDown => Some(0x79),
        KeyCode::PageUp => Some(0x74),
        KeyCode::Pause => None,
        KeyCode::Space => Some(0x31),
        KeyCode::Tab => Some(0x30),
        KeyCode::NumLock => Some(0x47),
        KeyCode::CapsLock => Some(0x39),
        KeyCode::ScrollLock => None,
        KeyCode::LeftShift => Some(0x38),
        KeyCode::RightShift => Some(0x3c),
        KeyCode::LeftCtrl => Some(0x3b),
        KeyCode::RightCtrl => Some(0x3e),
        KeyCode::NumPad0 => Some(0x52),
        KeyCode::NumPad1 => Some(0x53),
        KeyCode::NumPad2 => Some(0x54),
        KeyCode::NumPad3 => Some(0x55),
        KeyCode::NumPad4 => Some(0x56),
        KeyCode::NumPad5 => Some(0x57),
        KeyCode::NumPad6 => Some(0x58),
        KeyCode::NumPad7 => Some(0x59),
        KeyCode::NumPad8 => Some(0x5b),
        KeyCode::NumPad9 => Some(0x5c),
        KeyCode::NumPadDot => Some(0x41),
        KeyCode::NumPadSlash => Some(0x4b),
        KeyCode::NumPadAsterisk => Some(0x43),
        KeyCode::NumPadMinus => Some(0x4e),
        KeyCode::NumPadPlus => Some(0x45),
        KeyCode::NumPadEnter => Some(0x4c),
        KeyCode::LeftAlt => Some(0x3a),
        KeyCode::RightAlt => Some(0x3d),
        KeyCode::LeftSuper => Some(0x37),
        KeyCode::RightSuper => Some(0x36),
    }
}

#[derive(Default)]
struct Pending {
    events: Vec<(KeyCode, bool)>,
    overflow: bool,
}
#[derive(Clone, Default)]
pub struct KeyboardQueue(Arc<Mutex<Pending>>);
impl minifb::InputCallback for KeyboardQueue {
    fn add_char(&mut self, _: u32) {}
    fn set_key_state(&mut self, key: minifb::Key, down: bool) {
        if let Some(key) = from_native(key) {
            self.record(key, down);
        }
    }
}
impl KeyboardQueue {
    fn record(&self, key: KeyCode, down: bool) {
        let mut pending = self.0.lock().unwrap();
        if pending.events.len() < 1024 {
            pending.events.push((key, down));
        } else {
            pending.overflow = true;
        }
    }
    pub fn drain(&self) -> anyhow::Result<Vec<(KeyCode, bool)>> {
        let mut pending = self.0.lock().unwrap();
        anyhow::ensure!(!pending.overflow, "键盘事件队列已满，请重新连接");
        Ok(std::mem::take(&mut pending.events))
    }
}

#[derive(Default)]
pub struct KeyboardTracker {
    held: BTreeSet<KeyCode>,
    blocked: BTreeSet<KeyCode>,
}
impl KeyboardTracker {
    pub fn update(
        &mut self,
        events: Vec<(KeyCode, bool)>,
        active: bool,
        sequence: u64,
    ) -> Vec<Message> {
        let mut output = Vec::new();
        if !active && !self.held.is_empty() {
            self.blocked.append(&mut self.held);
            output.push(Message::ReleaseKeyboard);
        }
        for (key, down) in events {
            if !down {
                self.blocked.remove(&key);
            }
            if !active {
                if down {
                    self.blocked.insert(key);
                }
                continue;
            }
            if self.blocked.contains(&key) {
                continue;
            }
            if down && self.blocked.iter().any(|key| key.is_modifier()) {
                self.blocked.insert(key);
                continue;
            }
            if down {
                let fresh = self.held.insert(key);
                if !fresh && !key.repeatable() {
                    continue;
                }
            } else if !self.held.remove(&key) {
                continue;
            }
            output.push(Message::Key {
                event: KeyEvent {
                    frame_sequence: sequence,
                    key,
                    down,
                },
            });
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(key: KeyCode, down: bool) -> Message {
        Message::Key {
            event: KeyEvent {
                frame_sequence: 7,
                key,
                down,
            },
        }
    }
    #[test]
    fn quick_taps_and_chords_preserve_event_order() {
        let mut tracker = KeyboardTracker::default();
        let input = vec![
            (KeyCode::LeftCtrl, true),
            (KeyCode::A, true),
            (KeyCode::A, false),
            (KeyCode::LeftCtrl, false),
            (KeyCode::Escape, true),
            (KeyCode::Escape, false),
        ];
        let expected: Vec<_> = input.iter().map(|&(key, down)| event(key, down)).collect();
        assert_eq!(tracker.update(input, true, 7), expected);
    }
    #[test]
    fn repeats_text_keys_but_not_modifiers_or_lock_toggles() {
        let mut tracker = KeyboardTracker::default();
        assert_eq!(
            tracker.update(
                vec![
                    (KeyCode::LeftShift, true),
                    (KeyCode::LeftShift, true),
                    (KeyCode::A, true),
                    (KeyCode::A, true),
                    (KeyCode::A, false),
                    (KeyCode::CapsLock, true),
                    (KeyCode::CapsLock, true)
                ],
                true,
                7
            ),
            vec![
                event(KeyCode::LeftShift, true),
                event(KeyCode::A, true),
                event(KeyCode::A, true),
                event(KeyCode::A, false),
                event(KeyCode::CapsLock, true)
            ]
        );
    }
    #[test]
    fn focus_loss_releases_and_does_not_repress_held_modifiers() {
        let mut tracker = KeyboardTracker::default();
        tracker.update(vec![(KeyCode::LeftCtrl, true)], true, 7);
        assert_eq!(
            tracker.update(vec![], false, 7),
            vec![Message::ReleaseKeyboard]
        );
        assert!(tracker.update(vec![], false, 7).is_empty());
        assert!(
            tracker
                .update(vec![(KeyCode::LeftCtrl, true)], true, 7)
                .is_empty()
        );
        assert!(
            tracker
                .update(vec![(KeyCode::A, true), (KeyCode::A, false)], true, 7)
                .is_empty(),
            "do not turn a chord with a blocked modifier into plain text"
        );
        assert!(
            tracker
                .update(vec![(KeyCode::LeftCtrl, false)], true, 7)
                .is_empty()
        );
        assert_eq!(
            tracker.update(vec![(KeyCode::LeftCtrl, true)], true, 7),
            vec![event(KeyCode::LeftCtrl, true)]
        );
    }
    #[test]
    fn command_maps_to_windows_and_extended_keys_remain_distinct() {
        assert_eq!(
            windows_key(from_native(minifb::Key::LeftSuper).unwrap()),
            (0x5b, true)
        );
        assert_eq!(
            windows_key(from_native(minifb::Key::RightSuper).unwrap()),
            (0x5c, true)
        );
        assert_eq!(windows_key(KeyCode::LeftCtrl), (0xa2, false));
        assert_eq!(windows_key(KeyCode::RightCtrl), (0xa3, true));
        assert_eq!(windows_key(KeyCode::Enter), (13, false));
        assert_eq!(windows_key(KeyCode::NumPadEnter), (13, true));
        assert_eq!(windows_key(KeyCode::Left), (0x25, true));
    }
    #[test]
    fn callback_keeps_a_tap_that_starts_and_ends_between_frames() {
        use minifb::InputCallback;
        let mut queue = KeyboardQueue::default();
        queue.set_key_state(minifb::Key::A, true);
        queue.set_key_state(minifb::Key::A, false);
        assert_eq!(
            queue.drain().unwrap(),
            vec![(KeyCode::A, true), (KeyCode::A, false)]
        );
        assert!(queue.drain().unwrap().is_empty());
    }
}

// minifb's Windows scan-code table omits Alt, Win and F13-F15. Intercept only
// these messages on this viewer window; other keys keep the normal callback.
#[cfg(target_os = "windows")]
pub mod windows_capture {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::{
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    };
    pub struct Capture {
        window: HWND,
        queue: Box<KeyboardQueue>,
    }
    impl Capture {
        pub fn attach(window: &minifb::Window, queue: KeyboardQueue) -> anyhow::Result<Self> {
            let capture = Self {
                window: window.get_window_handle(),
                queue: Box::new(queue),
            };
            let data = (&*capture.queue as *const KeyboardQueue) as usize;
            anyhow::ensure!(
                unsafe { SetWindowSubclass(capture.window, Some(procedure), data, data) } != 0,
                "无法启用 Windows 预览窗口键盘捕获"
            );
            Ok(capture)
        }
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            let data = (&*self.queue as *const KeyboardQueue) as usize;
            unsafe {
                RemoveWindowSubclass(self.window, Some(procedure), data);
            }
        }
    }
    unsafe extern "system" fn procedure(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        data: usize,
    ) -> LRESULT {
        if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP) {
            let key = match wparam as u16 {
                0x5b => Some(KeyCode::LeftSuper),
                0x5c => Some(KeyCode::RightSuper),
                0xa4 => Some(KeyCode::LeftAlt),
                0xa5 => Some(KeyCode::RightAlt),
                0x12 => Some(if lparam & (1 << 24) == 0 {
                    KeyCode::LeftAlt
                } else {
                    KeyCode::RightAlt
                }),
                0x7c => Some(KeyCode::F13),
                0x7d => Some(KeyCode::F14),
                0x7e => Some(KeyCode::F15),
                _ => None,
            };
            if let Some(key) = key {
                // The boxed queue lives until Capture removes this subclass.
                let queue = unsafe { &*(data as *const KeyboardQueue) };
                queue.record(key, matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN));
                return 0;
            }
        }
        if message == WM_NCDESTROY {
            unsafe {
                RemoveWindowSubclass(window, Some(procedure), id);
            }
        }
        unsafe { DefSubclassProc(window, message, wparam, lparam) }
    }
}
