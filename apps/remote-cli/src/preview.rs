use anyhow::{Context, Result, bail, ensure};
use remote_protocol::{PreviewFrame, read_frame};
use remote_session::IO_TIMEOUT;
use std::{io::Cursor, path::Path};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

#[derive(Debug)]
pub struct DecodedFrame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    pub sequence: u64,
}

pub fn decode(frame: &PreviewFrame) -> Result<DecodedFrame> {
    remote_protocol::validate_frame_size(frame.width, frame.height)?;
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(&frame.jpeg));
    decoder.set_max_decoding_buffer_size(remote_protocol::MAX_FRAME_PIXELS * 4);
    decoder.read_info().context("无法读取 JPEG 帧头")?;
    let info = decoder.info().context("缺少 JPEG 帧头")?;
    ensure!(
        u32::from(info.width) == frame.width && u32::from(info.height) == frame.height,
        "帧头与 JPEG 尺寸不一致"
    );
    ensure!(
        info.pixel_format == jpeg_decoder::PixelFormat::RGB24,
        "不支持的 JPEG 像素格式"
    );
    let rgb = decoder.decode().context("无法解码窗口画面")?;
    let pixels = rgb
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
        .collect();
    Ok(DecodedFrame {
        width: frame.width as usize,
        height: frame.height as usize,
        pixels,
        sequence: frame.sequence,
    })
}

async fn next_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Result<PreviewFrame> {
    timeout(IO_TIMEOUT, read_frame(stream))
        .await
        .context("等待窗口画面超时")??
        .context("Mac 窗口采集已停止；请确认 Mac 已解锁、目标窗口仍打开，再重新启动共享")
}

// Reading must continue while JPEG decoding is busy: retain one compressed
// frame, not a backlog of obsolete images in front of the decoder.
#[cfg(any(target_os = "windows", target_os = "macos"))]
async fn decode_latest_frames<F>(
    mut incoming: tokio::sync::watch::Receiver<Result<std::sync::Arc<PreviewFrame>, String>>,
    output: tokio::sync::watch::Sender<Result<std::sync::Arc<DecodedFrame>, String>>,
    decode_frame: F,
) where
    F: Fn(&PreviewFrame) -> Result<DecodedFrame> + Send + Sync + 'static,
{
    let decode_frame = std::sync::Arc::new(decode_frame);
    while incoming.changed().await.is_ok() {
        let frame = incoming.borrow_and_update().clone();
        let result = match frame {
            Ok(frame) => {
                let decoder = decode_frame.clone();
                match tokio::task::spawn_blocking(move || decoder(&frame)).await {
                    Ok(result) => result
                        .map(std::sync::Arc::new)
                        .map_err(|e| format!("{e:#}")),
                    Err(error) => Err(format!("画面解码任务失败：{error}")),
                }
            }
            Err(error) => Err(error),
        };
        let failed = result.is_err();
        if output.send(result).is_err() || failed {
            break;
        }
    }
}

pub async fn save_frame<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    path: &Path,
) -> Result<()> {
    let window = remote_session::start_preview(stream).await?;
    let frame = next_frame(stream).await?;
    let decoded = decode(&frame)?;
    ensure!(
        decoded.pixels.len() == decoded.width * decoded.height,
        "不完整的窗口画面"
    );
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&frame.jpeg)?;
    println!(
        "已收到 {} — {} 的画面，保存到 {}（{}×{}，帧 {}）",
        window.app,
        window.title,
        path.display(),
        frame.width,
        frame.height,
        decoded.sequence
    );
    Ok(())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn native_window(title: &str, frame: &DecodedFrame) -> Result<minifb::Window> {
    use minifb::{ScaleMode, Window, WindowOptions};
    let mut window = Window::new(
        title,
        frame.width,
        frame.height,
        WindowOptions {
            resize: true,
            scale_mode: ScaleMode::AspectRatioStretch,
            ..WindowOptions::default()
        },
    )
    .context("无法创建预览窗口")?;
    window.set_target_fps(60);
    Ok(window)
}

#[cfg(target_os = "macos")]
mod macos_image {
    use super::*;
    use std::ffi::c_void;

    unsafe extern "C" {
        fn remote_preview_create(window: *mut c_void) -> *mut c_void;
        fn remote_preview_present(
            view: *mut c_void,
            pixels: *const u32,
            width: usize,
            height: usize,
        ) -> bool;
        fn remote_preview_mouse(
            view: *mut c_void,
            x: *mut f32,
            y: *mut f32,
            width: *mut usize,
            height: *mut usize,
        ) -> bool;
        fn remote_preview_keyboard_enabled(view: *mut c_void, enabled: bool);
        fn remote_preview_destroy(view: *mut c_void);
    }
    pub struct Renderer {
        view: *mut c_void,
        last_frame: Option<(u64, usize, usize)>,
    }
    impl Renderer {
        pub fn new(window: &minifb::Window) -> Result<Self> {
            // Window creation, rendering and destruction all run on the main thread.
            let view = unsafe { remote_preview_create(window.get_window_handle()) };
            ensure!(!view.is_null(), "无法创建 macOS 图像层");
            Ok(Self {
                view,
                last_frame: None,
            })
        }
        pub fn enable_keyboard(&self, enabled: bool) {
            unsafe {
                remote_preview_keyboard_enabled(self.view, enabled);
            }
        }
        pub fn mouse_geometry(&self) -> (Option<(f32, f32)>, (usize, usize)) {
            let (mut x, mut y, mut width, mut height) = (0.0, 0.0, 0, 0);
            let available =
                unsafe { remote_preview_mouse(self.view, &mut x, &mut y, &mut width, &mut height) };
            (available.then_some((x, y)), (width, height))
        }
        pub fn present(&mut self, window: &mut minifb::Window, frame: &DecodedFrame) -> Result<()> {
            let key = (frame.sequence, frame.width, frame.height);
            if self.last_frame != Some(key) {
                ensure!(
                    frame.pixels.len() == frame.width * frame.height,
                    "不完整的窗口画面"
                );
                ensure!(
                    unsafe {
                        remote_preview_present(
                            self.view,
                            frame.pixels.as_ptr(),
                            frame.width,
                            frame.height,
                        )
                    },
                    "无法绘制 macOS 预览画面"
                );
                self.last_frame = Some(key);
            }
            // Poll the original native window for input without drawing its
            // nearest-neighbour Metal surface over the smooth image layer.
            window.update();
            Ok(())
        }
    }
    impl Drop for Renderer {
        fn drop(&mut self) {
            unsafe { remote_preview_destroy(self.view) };
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn viewer_active(window: &minifb::Window) -> bool {
    #[cfg(target_os = "macos")]
    {
        // minifb 0.29's macOS is_active() reverses the native boolean.
        // Query the actual NSWindow rather than depending on that wrapper.
        // The handle is minifb's live NSWindow and this runs on the main thread.
        let native = unsafe { &*window.get_window_handle().cast::<objc2_app_kit::NSWindow>() };
        native.isKeyWindow()
    }
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, IsIconic, IsWindowVisible,
        };
        let handle = window.get_window_handle();
        // minifb queries GetActiveWindow, which is scoped to this thread and
        // can retain a window after its application loses foreground focus.
        unsafe {
            GetForegroundWindow() == handle && IsWindowVisible(handle) != 0 && IsIconic(handle) == 0
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub async fn view<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut stream: S,
    control: Option<S>,
) -> Result<()> {
    use crate::keyboard::{KeyboardQueue, KeyboardTracker};
    use crate::mouse::{ControlClient, MouseSample, MouseTracker, Status};
    use minifb::MouseButton;
    #[cfg(target_os = "windows")]
    use minifb::MouseMode;
    use std::{sync::Arc, time::Duration};
    use tokio::sync::watch;
    let shared = remote_session::start_preview(&mut stream).await?;
    let first_raw = Arc::new(next_frame(&mut stream).await?);
    let first = Arc::new(decode(&first_raw)?);
    let input = if let Some(control) = control {
        Some(ControlClient::start(control, shared.id).await?)
    } else {
        None
    };
    let title = format!(
        "{} — {}（{}，关闭窗口退出）",
        shared.app,
        shared.title,
        if input.is_some() {
            "键鼠控制"
        } else {
            "仅观看"
        }
    );
    let mut window = native_window(&title, &first)?;
    let keyboard_queue = KeyboardQueue::default();
    if input.is_some() {
        window.set_input_callback(Box::new(keyboard_queue.clone()));
    }
    #[cfg(target_os = "windows")]
    let _keyboard_capture = if input.is_some() {
        Some(crate::keyboard::windows_capture::Capture::attach(
            &window,
            keyboard_queue.clone(),
        )?)
    } else {
        None
    };
    let mut keyboard = KeyboardTracker::default();
    #[cfg(target_os = "macos")]
    let mut renderer = macos_image::Renderer::new(&window)?;
    #[cfg(target_os = "macos")]
    renderer.enable_keyboard(input.is_some());
    let (sender, receiver) = watch::channel(Ok::<_, String>(first));
    let (compressed_tx, compressed_rx) = watch::channel(Ok(first_raw));
    let decoder = tokio::spawn(decode_latest_frames(compressed_rx, sender, decode));
    let reader = tokio::spawn(async move {
        loop {
            let result = next_frame(&mut stream)
                .await
                .map(Arc::new)
                .map_err(|error| format!("{error:#}"));
            let failed = result.is_err();
            if compressed_tx.send(result).is_err() || failed {
                break;
            }
        }
    });
    // The native window stays on the main thread; Tokio workers receive and
    // decode independently. Both stages retain only the most recent frame.
    let mut mouse = MouseTracker::default();
    let mut notice = None;
    let mut displayed_dimensions = None;
    let result = (|| -> Result<()> {
        while window.is_open() {
            let current = receiver.borrow().clone();
            match current {
                Ok(frame) => {
                    let dimensions = (frame.width, frame.height);
                    if displayed_dimensions != Some(dimensions) {
                        window.set_title(&format!("{title} — {}×{}", frame.width, frame.height));
                        println!("收到共享画面：{}×{} 像素。", frame.width, frame.height);
                        displayed_dimensions = Some(dimensions);
                    }
                    #[cfg(target_os = "macos")]
                    renderer.present(&mut window, &frame)?;
                    #[cfg(target_os = "windows")]
                    window.update_with_buffer(&frame.pixels, frame.width, frame.height)?;
                    if let Some(input) = &input {
                        match input.status() {
                            Status::Failed(error) => bail!("{error}"),
                            Status::Notice(error) => {
                                if notice.as_ref() != Some(&error) {
                                    eprintln!("{error}");
                                    window.set_title(&format!(
                                        "{title} — {}×{} — {error}",
                                        frame.width, frame.height
                                    ));
                                    notice = Some(error);
                                }
                            }
                            Status::Ready => {
                                if notice.take().is_some() {
                                    window.set_title(&format!(
                                        "{title} — {}×{}",
                                        frame.width, frame.height
                                    ));
                                }
                            }
                        }
                        let active = viewer_active(&window);
                        for event in
                            keyboard.update(keyboard_queue.drain()?, active, frame.sequence)
                        {
                            input.send(event)?;
                        }
                        #[cfg(target_os = "macos")]
                        let (position, size) = renderer.mouse_geometry();
                        #[cfg(target_os = "windows")]
                        let (position, size) = (
                            window.get_unscaled_mouse_pos(MouseMode::Discard),
                            window.get_size(),
                        );
                        for event in mouse.update(MouseSample {
                            position,
                            window: size,
                            frame: (frame.width, frame.height),
                            sequence: frame.sequence,
                            buttons: [
                                window.get_mouse_down(MouseButton::Left),
                                window.get_mouse_down(MouseButton::Right),
                                window.get_mouse_down(MouseButton::Middle),
                            ],
                            scroll: window.get_scroll_wheel(),
                            active,
                        }) {
                            input.send(event)?;
                        }
                    }
                }
                Err(error) => bail!("{error}"),
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    })();
    reader.abort();
    decoder.abort();
    if let Some(input) = input {
        input.finish().await;
    }
    result
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub async fn view<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    _stream: S,
    _control: Option<S>,
) -> Result<()> {
    bail!("图形预览支持 macOS 和 Windows；当前平台可用 --save-frame preview.jpg 或 --check-only")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> PreviewFrame {
        let mut jpeg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpeg, 90)
            .encode(&[255, 0, 0].repeat(16), 4, 4, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        PreviewFrame {
            width: 4,
            height: 4,
            sequence: 1,
            jpeg,
        }
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[tokio::test]
    async fn slow_decoder_skips_pending_old_frames_and_preserves_stop() {
        use std::sync::{Arc, Mutex};
        use tokio::sync::{Notify, watch};
        let raw = Arc::new(image());
        let (tx, rx) = watch::channel(Ok(raw.clone()));
        let (output, mut received) = watch::channel(Ok(Arc::new(decode(&raw).unwrap())));
        let started = Arc::new(Notify::new());
        let notification = started.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let wait = Mutex::new(wait);
        let task = tokio::spawn(decode_latest_frames(rx, output, move |frame| {
            if frame.sequence == 2 {
                notification.notify_one();
                wait.lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .unwrap();
            }
            decode(frame)
        }));
        let send = |sequence| {
            let mut frame = image();
            frame.sequence = sequence;
            tx.send(Ok(Arc::new(frame))).unwrap();
        };
        send(2);
        timeout(std::time::Duration::from_secs(3), started.notified())
            .await
            .unwrap();
        send(3);
        send(4);
        release.send(()).unwrap();
        timeout(std::time::Duration::from_secs(3), async {
            loop {
                received.changed().await.unwrap();
                let sequence = received.borrow_and_update().as_ref().unwrap().sequence;
                assert_ne!(sequence, 3, "obsolete pending frame was decoded");
                if sequence == 4 {
                    break;
                }
            }
            tx.send(Err("capture stopped".into())).unwrap();
            received.changed().await.unwrap();
            assert_eq!(
                received.borrow_and_update().as_ref().unwrap_err(),
                "capture stopped"
            );
            task.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[test]
    fn decodes_bounded_jpeg_to_native_rgb_buffer() {
        let frame = decode(&image()).unwrap();
        assert_eq!((frame.width, frame.height, frame.pixels.len()), (4, 4, 16));
        assert!((frame.pixels[0] >> 16) > 240);
        assert!((frame.pixels[0] & 0xff) < 16);
    }

    #[test]
    fn decodes_full_resolution_portrait() {
        let mut jpeg = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut jpeg, 95);
        encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4);
        encoder
            .encode(
                &[255, 0, 0].repeat(1440 * 2560),
                1440,
                2560,
                jpeg_encoder::ColorType::Rgb,
            )
            .unwrap();
        let frame = decode(&PreviewFrame {
            width: 1440,
            height: 2560,
            sequence: 1,
            jpeg,
        })
        .unwrap();
        assert_eq!(
            (frame.width, frame.height, frame.pixels.len()),
            (1440, 2560, 1440 * 2560)
        );
    }

    #[test]
    fn rejects_dimension_mismatch_before_decoding_pixels() {
        let mut frame = image();
        frame.width = 1920;
        assert!(
            decode(&frame)
                .unwrap_err()
                .to_string()
                .contains("尺寸不一致")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_window_presents_a_decoded_frame() {
        let frame = decode(&image()).unwrap();
        let mut window = native_window("MyRemoteApp CI preview check", &frame).unwrap();
        window
            .update_with_buffer(&frame.pixels, frame.width, frame.height)
            .unwrap();
        assert!(window.is_open());
    }
}
