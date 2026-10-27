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

pub(crate) fn is_locked() -> bool {
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
    input: Option<Arc<super::input_macos::MacInput>>,
    paused: Arc<AtomicBool>,
    sender: watch::Sender<Option<CaptureEvent>>,
}

impl CaptureSession {
    pub fn paused_for_lock(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }
    pub fn pause_for_lock(&self) {
        if !self.paused.swap(true, Ordering::AcqRel) {
            if let Some(input) = &self.input {
                input.stop();
            }
            self.sender.send_replace(Some(CaptureEvent::Stopped(
                "Mac 已锁屏；重新连接后可输入密码解锁".into(),
            )));
            let _ = self.stream.stop_capture();
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        if let Some(input) = &self.input {
            input.stop();
        }
        let _ = self.stream.stop_capture();
    }
}

pub fn start(window_id: Option<u32>, view_only: bool) -> Result<CaptureSession> {
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
    let input = if view_only {
        None
    } else {
        super::input_macos::require_permission()?;
        let pid = window
            .owning_application()
            .context("目标 App 已退出")?
            .process_id();
        Some(super::input_macos::MacInput::new(selected.id, pid))
    };
    let filter = SCContentFilter::create().with_window(window).build()?;
    let rect = filter.content_rect();
    let (width, height) = capture_dimensions(
        rect.size.width,
        rect.size.height,
        f64::from(filter.point_pixel_scale()),
    );
    println!("共享画面：{width}×{height}，10 FPS，高清文字模式。");
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
    let paused = Arc::new(AtomicBool::new(false));
    let paused_error = paused.clone();
    let stopped_input = input.clone();
    let delegate = StreamCallbacks::new().on_error(move |error| {
        if let Some(input) = &stopped_input {
            input.stop();
        }
        if is_locked() {
            paused_error.store(true, Ordering::Release);
        }
        eprintln!("窗口采集停止：{error}");
        errors.send_replace(Some(CaptureEvent::Stopped(error.to_string())));
    });
    let mut stream = SCStream::new_with_delegate(&filter, &configuration, delegate)?;
    stream.add_output_handler(
        Handler {
            sender: sender.clone(),
            paused: paused.clone(),
            busy: Mutex::new(()),
            sequence: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            input: input.clone(),
        },
        SCStreamOutputType::Screen,
    )?;
    stream
        .start_capture()
        .context("无法采集指定窗口，请检查屏幕录制权限或重新选择窗口")?;
    println!(
        "正在共享：{} — {}（{}×{}，最多 10 FPS）",
        selected.app, selected.title, width, height
    );
    Ok(CaptureSession {
        stream,
        input: input.clone(),
        paused,
        sender,
        source: PreviewSource {
            window: WindowInfo {
                width,
                height,
                ..selected
            },
            frames: receiver,
            input: input.map(|input| {
                Arc::new(super::input_macos::Controller(input))
                    as Arc<dyn remote_session::InputController>
            }),
        },
    })
}

struct Handler {
    sender: watch::Sender<Option<CaptureEvent>>,
    busy: Mutex<()>,
    sequence: AtomicU64,
    stopped: AtomicBool,
    paused: Arc<AtomicBool>,
    input: Option<Arc<super::input_macos::MacInput>>,
}

impl SCStreamOutputTrait for Handler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, kind: SCStreamOutputType) {
        if kind != SCStreamOutputType::Screen || self.stopped.load(Ordering::Relaxed) {
            return;
        }
        if is_locked() {
            self.paused.store(true, Ordering::Release);
            if !self.stopped.swap(true, Ordering::Relaxed) {
                if let Some(input) = &self.input {
                    input.stop();
                }
                let reason = "Mac 已锁屏；重新连接后可输入密码解锁";
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
        let mut encoder = jpeg_encoder::Encoder::new(&mut jpeg, 95);
        // Preserve full chroma resolution: subsampling smears colored UI text.
        encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4);
        encoder.encode(
            &packed,
            width as u16,
            height as u16,
            jpeg_encoder::ColorType::Bgra,
        )?;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        if let Some(input) = &self.input
            && let Some(content) = sample.content_rect()
        {
            let scale = sample.scale_factor().unwrap_or(1.0);
            input.record(
                sequence,
                width as u32,
                height as u32,
                super::input_macos::Rect {
                    origin: super::input_macos::Point {
                        x: content.origin.x * scale,
                        y: content.origin.y * scale,
                    },
                    size: super::input_macos::Size {
                        width: content.size.width * scale,
                        height: content.size.height * scale,
                    },
                },
            );
        }
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

// Bound work by both orientation and pixel count, retaining native Retina pixels
// for ordinary app windows instead of first reducing them to logical points.
fn capture_dimensions(width: f64, height: f64, scale: f64) -> (u32, u32) {
    let scale = if scale.is_finite() && scale >= 1.0 {
        scale
    } else {
        1.0
    };
    let width = width.max(1.0) * scale;
    let height = height.max(1.0) * scale;
    let ratio = (2560.0 / width.max(height))
        .min(1440.0 / width.min(height))
        .min(1.0);
    (
        (width * ratio).round().max(1.0) as u32,
        (height * ratio).round().max(1.0) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::capture_dimensions;

    #[test]
    fn keeps_retina_detail_and_bounds_both_orientations() {
        assert_eq!(capture_dimensions(420.0, 702.0, 2.0), (840, 1404));
        assert_eq!(capture_dimensions(420.0, 702.0, 1.0), (420, 702));
        assert_eq!(capture_dimensions(1920.0, 1080.0, 2.0), (2560, 1440));
        assert_eq!(capture_dimensions(1080.0, 1920.0, 2.0), (1440, 2560));
        assert_eq!(capture_dimensions(1000.0, 1000.0, 2.0), (1440, 1440));
    }
}
