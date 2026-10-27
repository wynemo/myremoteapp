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
    window.is_active()
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub async fn view<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut stream: S,
    control: Option<S>,
) -> Result<()> {
    use crate::mouse::{ControlClient, MouseSample, MouseTracker, Status};
    use minifb::{Key, MouseButton, MouseMode};
    use std::{sync::Arc, time::Duration};
    use tokio::sync::watch;
    let shared = remote_session::start_preview(&mut stream).await?;
    let first = Arc::new(decode(&next_frame(&mut stream).await?)?);
    let input = if let Some(control) = control {
        Some(ControlClient::start(control, shared.id).await?)
    } else {
        None
    };
    let title = format!(
        "{} — {}（{}，Esc 关闭）",
        shared.app,
        shared.title,
        if input.is_some() {
            "鼠标控制；键盘暂不支持"
        } else {
            "仅观看"
        }
    );
    let mut window = native_window(&title, &first)?;
    let (sender, receiver) = watch::channel(Ok::<_, String>(first));
    let reader = tokio::spawn(async move {
        loop {
            match next_frame(&mut stream)
                .await
                .and_then(|frame| decode(&frame))
            {
                Ok(frame) => {
                    if sender.send(Ok(Arc::new(frame))).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(format!("{error:#}")));
                    break;
                }
            }
        }
    });
    // The native window stays on the main thread; Tokio workers receive and
    // decode independently. Both stages retain only the most recent frame.
    let mut mouse = MouseTracker::default();
    let mut notice = None;
    let mut displayed_dimensions = None;
    let result = (|| -> Result<()> {
        while window.is_open() && !window.is_key_down(Key::Escape) {
            let current = receiver.borrow().clone();
            match current {
                Ok(frame) => {
                    let dimensions = (frame.width, frame.height);
                    if displayed_dimensions != Some(dimensions) {
                        window.set_title(&format!("{title} — {}×{}", frame.width, frame.height));
                        println!("收到共享画面：{}×{} 像素。", frame.width, frame.height);
                        displayed_dimensions = Some(dimensions);
                    }
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
                        for event in mouse.update(MouseSample {
                            position: window.get_unscaled_mouse_pos(MouseMode::Discard),
                            window: window.get_size(),
                            frame: (frame.width, frame.height),
                            sequence: frame.sequence,
                            buttons: [
                                window.get_mouse_down(MouseButton::Left),
                                window.get_mouse_down(MouseButton::Right),
                                window.get_mouse_down(MouseButton::Middle),
                            ],
                            scroll: window.get_scroll_wheel(),
                            active: viewer_active(&window),
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
