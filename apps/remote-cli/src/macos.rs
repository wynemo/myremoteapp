use anyhow::{Context, Result, bail, ensure};
use remote_protocol::{PreviewFrame, WindowInfo};
use remote_session::{CaptureEvent, PreviewSource};
use screencapturekit::{
    cm::{CMSampleBufferSCExt, SCFrameStatus},
    cv::CVPixelBufferLockFlags,
    prelude::*,
    stream::delegate_trait::StreamCallbacks,
};
use std::{
    io::{self, IsTerminal, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::watch;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn sc_initialize_core_graphics();
    fn CGSessionCopyCurrentDictionary() -> *const std::ffi::c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const std::ffi::c_void,
        text: *const std::ffi::c_char,
        encoding: u32,
    ) -> *const std::ffi::c_void;
    fn CFDictionaryGetValue(
        dictionary: *const std::ffi::c_void,
        key: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
    fn CFBooleanGetValue(value: *const std::ffi::c_void) -> bool;
    fn CFRelease(value: *const std::ffi::c_void);
}

fn is_locked() -> bool {
    // The session dictionary exposes the desktop lock flag as a CFBoolean.
    // Both objects created with Copy/Create are released on every branch.
    unsafe {
        let dictionary = CGSessionCopyCurrentDictionary();
        if dictionary.is_null() {
            return true;
        }
        let key = CFStringCreateWithCString(
            std::ptr::null(),
            c"CGSSessionScreenIsLocked".as_ptr(),
            0x0800_0100,
        );
        if key.is_null() {
            CFRelease(dictionary);
            return true;
        }
        let value = CFDictionaryGetValue(dictionary, key);
        let locked = !value.is_null() && CFBooleanGetValue(value);
        CFRelease(key);
        CFRelease(dictionary);
        locked
    }
}

fn clean(text: &str) -> String {
    text.chars()
        .take(240)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn available() -> Result<(SCShareableContent, Vec<WindowInfo>)> {
    // CoreGraphics must be initialized before the Swift bridge queries windows.
    unsafe {
        sc_initialize_core_graphics();
    }
    if !unsafe { CGPreflightScreenCaptureAccess() } && !unsafe { CGRequestScreenCaptureAccess() } {
        bail!(
            "需要屏幕录制权限：在 系统设置 → 隐私与安全性 → 屏幕与系统音频录制 中允许当前终端或应用，然后重新启动程序"
        );
    }
    ensure!(!is_locked(), "Mac 已锁屏，请解锁后重新运行窗口共享命令");
    let content = SCShareableContent::get().context("无法获取窗口列表，请检查屏幕录制权限")?;
    let snapshot = content.snapshot().context("无法读取窗口信息")?;
    let mut windows: Vec<_> = snapshot
        .windows
        .iter()
        .filter_map(|window| {
            if !window.is_on_screen
                || window.window_layer != 0
                || window.frame.size.width < 32.0
                || window.frame.size.height < 32.0
            {
                return None;
            }
            let app = snapshot.applications.get(window.owning_app_index?)?;
            Some(WindowInfo {
                id: window.window_id,
                app: clean(&app.application_name),
                title: clean(window.title.as_deref().unwrap_or("（无标题）")),
                width: window.frame.size.width as u32,
                height: window.frame.size.height as u32,
            })
        })
        .collect();
    windows.sort_by(|a, b| (&a.app, &a.title, a.id).cmp(&(&b.app, &b.title, b.id)));
    ensure!(
        !windows.is_empty(),
        "没有可共享的窗口，请先打开目标 App 窗口"
    );
    Ok((content, windows))
}

fn print_windows(windows: &[WindowInfo]) {
    for (index, window) in windows.iter().enumerate() {
        println!(
            "{}. {} — {}（ID {}，{}×{}）",
            index + 1,
            window.app,
            window.title,
            window.id,
            window.width,
            window.height
        );
    }
}

pub fn list_windows() -> Result<()> {
    let (_, windows) = available()?;
    print_windows(&windows);
    Ok(())
}

pub struct CaptureSession {
    stream: SCStream,
    pub source: PreviewSource,
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        let _ = self.stream.stop_capture();
    }
}

pub fn start(window_id: Option<u32>) -> Result<CaptureSession> {
    let (content, windows) = available()?;
    let selected = if let Some(id) = window_id {
        windows
            .iter()
            .find(|window| window.id == id)
            .context("窗口不存在或已关闭，请运行 windows 查看当前窗口 ID")?
            .clone()
    } else {
        print_windows(&windows);
        ensure!(
            io::stdin().is_terminal(),
            "请在交互式终端选择窗口，或通过 --window-id ID 指定；仅检查连接可用 --check-only"
        );
        loop {
            print!("选择要共享的窗口编号（q 退出）：");
            io::stdout().flush()?;
            let mut input = String::new();
            ensure!(io::stdin().read_line(&mut input)? != 0, "未选择窗口");
            if input.trim().eq_ignore_ascii_case("q") {
                bail!("已取消共享");
            }
            if let Ok(index) = input.trim().parse::<usize>()
                && let Some(window) = index.checked_sub(1).and_then(|index| windows.get(index))
            {
                break window.clone();
            }
            println!("请输入列表中的编号。");
        }
    };
    let live = content.windows();
    let window = live
        .iter()
        .find(|window| window.window_id() == selected.id)
        .context("选中的窗口已关闭，请重新选择")?;
    let ratio = (1280.0 / selected.width as f64)
        .min(720.0 / selected.height as f64)
        .min(1.0);
    let width = (selected.width as f64 * ratio).round().max(1.0) as u32;
    let height = (selected.height as f64 * ratio).round().max(1.0) as u32;
    let filter = SCContentFilter::create().with_window(window).build()?;
    let configuration = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::BGRA)
        .with_minimum_frame_interval(&CMTime::new(1, 10))
        .with_shows_cursor(true)
        .with_captures_audio(false)
        .with_preserves_aspect_ratio(true)?
        .with_queue_depth(3);
    let (sender, receiver) = watch::channel(None);
    let errors = sender.clone();
    let delegate = StreamCallbacks::new().on_error(move |error| {
        eprintln!("窗口采集停止：{error}");
        errors.send_replace(Some(CaptureEvent::Stopped(error.to_string())));
    });
    let mut stream = SCStream::new_with_delegate(&filter, &configuration, delegate)?;
    stream.add_output_handler(
        Handler {
            sender,
            busy: Mutex::new(()),
            sequence: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
        },
        SCStreamOutputType::Screen,
    )?;
    stream
        .start_capture()
        .context("无法采集指定窗口，请检查屏幕录制权限或重新选择窗口")?;
    println!(
        "正在共享：{} — {}（{}×{}，最多 10 FPS，预览模式）",
        selected.app, selected.title, width, height
    );
    Ok(CaptureSession {
        stream,
        source: PreviewSource {
            window: WindowInfo {
                width,
                height,
                ..selected
            },
            frames: receiver,
        },
    })
}

struct Handler {
    sender: watch::Sender<Option<CaptureEvent>>,
    busy: Mutex<()>,
    sequence: AtomicU64,
    stopped: AtomicBool,
}

impl SCStreamOutputTrait for Handler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, kind: SCStreamOutputType) {
        if kind != SCStreamOutputType::Screen || self.stopped.load(Ordering::Relaxed) {
            return;
        }
        if is_locked() {
            if !self.stopped.swap(true, Ordering::Relaxed) {
                let reason = "Mac 已锁屏，请解锁后重新启动共享";
                eprintln!("{reason}");
                self.sender
                    .send_replace(Some(CaptureEvent::Stopped(reason.into())));
            }
            return;
        }
        if !matches!(
            sample.frame_status(),
            Some(SCFrameStatus::Complete | SCFrameStatus::Started)
        ) || sample.pixel_buffer().is_none()
        {
            return;
        }
        let Ok(_busy) = self.busy.try_lock() else {
            return;
        };
        let result = self.encode(sample);
        match result {
            Ok(frame) => {
                self.sender
                    .send_replace(Some(CaptureEvent::Frame(Arc::new(frame))));
            }
            Err(error) => {
                self.stopped.store(true, Ordering::Relaxed);
                eprintln!("窗口画面处理失败：{error:#}");
                self.sender
                    .send_replace(Some(CaptureEvent::Stopped(error.to_string())));
            }
        }
    }
}

impl Handler {
    fn encode(&self, sample: CMSampleBuffer) -> Result<PreviewFrame> {
        let pixel_buffer = sample.pixel_buffer().context("采集帧没有像素数据")?;
        let guard = pixel_buffer
            .lock(CVPixelBufferLockFlags::READ_ONLY)
            .map_err(|error| anyhow::anyhow!("锁定像素数据失败：{error}"))?;
        let width = guard.width();
        let height = guard.height();
        remote_protocol::validate_frame_size(width as u32, height as u32)?;
        let stride = guard.bytes_per_row();
        // BGRA configuration guarantees an interleaved buffer. The read-only
        // lock pins its storage until the guard is dropped after encoding.
        let bytes = unsafe { guard.as_slice() }.context("采集帧像素为空")?;
        ensure!(
            stride >= width * 4 && bytes.len() >= stride * height,
            "无效的像素行跨度"
        );
        let mut packed = Vec::with_capacity(width * height * 4);
        for row in bytes.chunks_exact(stride).take(height) {
            packed.extend_from_slice(&row[..width * 4]);
        }
        let mut jpeg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpeg, 75).encode(
            &packed,
            width as u16,
            height as u16,
            jpeg_encoder::ColorType::Bgra,
        )?;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        if sequence == 0 {
            println!("已取得首帧，Windows 可以连接观看。");
        }
        Ok(PreviewFrame {
            width: width as u32,
            height: height as u32,
            sequence,
            jpeg,
        })
    }
}
