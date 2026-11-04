use anyhow::{Result, ensure};
use arboard::{Clipboard, ImageData};
use remote_protocol::ClipboardFormat;
use remote_session::clipboard::{Backend, Content, MAX_IMAGE, MAX_PIXELS, MAX_TEXT};
use std::{borrow::Cow, io::Cursor, sync::Arc};

mod files;

#[derive(Default)]
pub struct Native {
    // Retain previous bundles too: an application may still be copying them.
    received: std::sync::Mutex<ReceivedFiles>,
}
#[derive(Default)]
struct ReceivedFiles {
    bundles: Vec<files::Staged>,
    bytes: usize,
}
impl Backend for Native {
    fn available(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            !crate::macos::is_locked()
        }
        #[cfg(target_os = "windows")]
        {
            crate::windows_host::current_desktop_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("Default"))
        }
    }
    fn version(&self) -> u64 {
        #[cfg(target_os = "macos")]
        unsafe {
            unsafe extern "C" {
                fn remote_clipboard_version() -> u64;
            }
            remote_clipboard_version()
        }
        #[cfg(target_os = "windows")]
        unsafe {
            u64::from(windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber())
        }
    }
    fn read(&self) -> Result<Option<Content>> {
        if !self.available() {
            return Ok(None);
        }
        let mut clipboard = Clipboard::new()?;
        if let Ok(paths) = clipboard.get().file_list() {
            return match files::pack(&paths) {
                Ok(bytes) => {
                    eprintln!(
                        "已读取剪贴板文件（{:.1} MiB），等待同步",
                        bytes.len() as f64 / (1024.0 * 1024.0)
                    );
                    Ok(Some(Content {
                        format: ClipboardFormat::Files,
                        bytes: Arc::new(bytes),
                    }))
                }
                Err(error) => {
                    eprintln!("剪贴板文件未发送：{error:#}");
                    Ok(None)
                }
            };
        }
        if let Ok(image) = clipboard.get_image() {
            if image
                .width
                .checked_mul(image.height)
                .is_none_or(|pixels| pixels > MAX_PIXELS)
            {
                return Ok(None);
            }
            let bytes = encode(&image)?;
            if bytes.len() <= MAX_IMAGE {
                return Ok(Some(Content {
                    format: ClipboardFormat::Png,
                    bytes: Arc::new(bytes),
                }));
            }
            return Ok(None);
        }
        if let Ok(text) = clipboard.get_text()
            && !text.is_empty()
            && text.len() <= MAX_TEXT
            && !text.contains('\0')
        {
            return Ok(Some(Content {
                format: ClipboardFormat::Text,
                bytes: Arc::new(text.into_bytes()),
            }));
        }
        Ok(None)
    }
    fn write(&self, expected: u64, content: &Content) -> Result<Option<u64>> {
        self.write_guarded(expected, content, &|| true)
    }
    fn write_guarded(
        &self,
        expected: u64,
        content: &Content,
        allowed: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<u64>> {
        content.validate()?;
        if content.format == ClipboardFormat::Files {
            let mut received = self.received.lock().unwrap();
            ensure!(
                received.bundles.len() < 32
                    && received.bytes + content.bytes.len() <= 256 * 1024 * 1024,
                "剪贴板文件暂存已满，请完成粘贴后重启本端程序"
            );
            let staged = files::unpack(&content.bytes)?;
            let mut clipboard = Clipboard::new()?;
            if !allowed() || !self.available() || self.version() != expected {
                return Ok(None);
            }
            clipboard.set().file_list(&staged.roots)?;
            received.bytes += content.bytes.len();
            received.bundles.push(staged);
            eprintln!("剪贴板文件已接收，可粘贴到 Finder / 资源管理器");
            return Ok(Some(self.version()));
        }
        // Decode before the final version check, as a large image can take time.
        let image = if content.format == ClipboardFormat::Png {
            Some(decode(&content.bytes)?)
        } else {
            None
        };
        let mut clipboard = Clipboard::new()?;
        if !allowed() || !self.available() || self.version() != expected {
            return Ok(None);
        }
        if let Some(image) = image {
            clipboard.set_image(image)?;
        } else {
            clipboard.set_text(std::str::from_utf8(&content.bytes)?)?;
        }
        Ok(Some(self.version()))
    }
}

fn encode(image: &ImageData<'_>) -> Result<Vec<u8>> {
    ensure!(
        image.width > 0
            && image.height > 0
            && image
                .width
                .checked_mul(image.height)
                .is_some_and(|n| n <= MAX_PIXELS),
        "剪贴板图片尺寸超限"
    );
    ensure!(
        image.bytes.len() == image.width * image.height * 4,
        "剪贴板图片数据不完整"
    );
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, image.width as u32, image.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&image.bytes)?;
    }
    Ok(bytes)
}
fn decode(bytes: &[u8]) -> Result<ImageData<'static>> {
    ensure!(bytes.len() <= MAX_IMAGE, "剪贴板图片超限");
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: MAX_PIXELS * 8,
        },
    );
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let info = reader.info();
    let (width, height) = (info.width as usize, info.height as usize);
    ensure!(
        width > 0 && height > 0 && width.checked_mul(height).is_some_and(|n| n <= MAX_PIXELS),
        "剪贴板图片尺寸超限"
    );
    ensure!(info.animation_control.is_none(), "不支持动画剪贴板图片");
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| anyhow::anyhow!("剪贴板图片尺寸溢出"))?;
    ensure!(size <= MAX_PIXELS * 4, "剪贴板图片解码超限");
    let mut pixels = vec![0; size];
    let output = reader.next_frame(&mut pixels)?;
    let source = &pixels[..output.buffer_size()];
    let rgba = match output.color_type {
        png::ColorType::Rgba => source.to_vec(),
        png::ColorType::Rgb => source
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => source.iter().flat_map(|p| [*p, *p, *p, 255]).collect(),
        png::ColorType::GrayscaleAlpha => source
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        _ => anyhow::bail!("不支持的剪贴板图片格式"),
    };
    ensure!(rgba.len() == width * height * 4, "剪贴板图片数据不完整");
    Ok(ImageData {
        width,
        height,
        bytes: Cow::Owned(rgba),
    })
}

struct ActiveBackend {
    active: Arc<std::sync::atomic::AtomicBool>,
    native: Native,
}
impl Backend for ActiveBackend {
    fn available(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Acquire) && self.native.available()
    }
    fn version(&self) -> u64 {
        self.native.version()
    }
    fn read(&self) -> Result<Option<Content>> {
        if self.available() {
            self.native.read()
        } else {
            Ok(None)
        }
    }
    fn write(&self, expected: u64, content: &Content) -> Result<Option<u64>> {
        if self.available() {
            self.native
                .write_guarded(expected, content, &|| self.available())
        } else {
            Ok(None)
        }
    }
}
pub struct Running {
    task: tokio::task::JoinHandle<()>,
    active: Arc<std::sync::atomic::AtomicBool>,
    pub progress: Arc<remote_session::clipboard::Progress>,
    pub backend: Arc<dyn Backend>,
}
impl Running {
    pub fn start<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>(
        stream: S,
        permissions: remote_session::clipboard::Permissions,
    ) -> Self {
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let backend: Arc<dyn Backend> = Arc::new(ActiveBackend {
            active: active.clone(),
            native: Native::default(),
        });
        let progress = Arc::new(remote_session::clipboard::Progress::default());
        let native = backend.clone();
        let state = progress.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = remote_session::clipboard::exchange_with_progress(
                stream,
                native,
                permissions,
                false,
                Some(state),
            )
            .await
            {
                eprintln!("剪贴板共享已停止：{error:#}");
            }
        });
        Self {
            task,
            active,
            progress,
            backend,
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.active
            .store(false, std::sync::atomic::Ordering::Release);
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn png_roundtrip_preserves_pixels_and_alpha() {
        let image = ImageData {
            width: 2,
            height: 1,
            bytes: Cow::Owned(vec![255, 0, 0, 0, 0, 128, 255, 127]),
        };
        let decoded = decode(&encode(&image).unwrap()).unwrap();
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.bytes, image.bytes);
    }
    #[test]
    fn rejects_invalid_and_oversized_images() {
        assert!(decode(b"not a PNG").is_err());
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, MAX_PIXELS as u32 + 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.write_header().unwrap().finish().unwrap();
        assert!(decode(&bytes).is_err());
    }
    #[test]
    #[ignore = "writes the system clipboard; run only on a disposable CI runner"]
    fn native_clipboard_roundtrip_on_disposable_runner() {
        assert!(
            std::env::var_os("REMOTEAPP_CLIPBOARD_NATIVE_TEST").is_some(),
            "native clipboard test is restricted to disposable runners"
        );
        let backend = Native::default();
        assert!(backend.available());
        let source = tempfile::tempdir().unwrap();
        let folder = source.path().join("中文🙂");
        std::fs::create_dir_all(folder.join("empty")).unwrap();
        std::fs::write(folder.join("binary.dat"), [0, 255, 13, 10]).unwrap();
        let file = source.path().join("zero.txt");
        std::fs::write(&file, []).unwrap();
        for content in [
            Content {
                format: ClipboardFormat::Text,
                bytes: Arc::new("中文🙂\nclipboard".as_bytes().to_vec()),
            },
            Content {
                format: ClipboardFormat::Files,
                bytes: Arc::new(files::pack(&[folder, file]).unwrap()),
            },
            Content {
                format: ClipboardFormat::Png,
                bytes: Arc::new(
                    encode(&ImageData {
                        width: 2,
                        height: 1,
                        bytes: Cow::Owned(vec![255, 0, 0, 255, 0, 128, 255, 255]),
                    })
                    .unwrap(),
                ),
            },
        ] {
            let version = backend.version();
            assert!(
                backend
                    .write_guarded(version, &content, &|| false)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(backend.version(), version);
            assert!(backend.write(version, &content).unwrap().is_some());
            let read = backend.read().unwrap().unwrap();
            assert_eq!(read.format, content.format);
            match read.format {
                ClipboardFormat::Text => assert_eq!(read.bytes, content.bytes),
                ClipboardFormat::Png => assert_eq!(
                    decode(&read.bytes).unwrap().bytes,
                    decode(&content.bytes).unwrap().bytes
                ),
                ClipboardFormat::Files => {
                    let staged = files::unpack(&read.bytes).unwrap();
                    assert_eq!(staged.roots.len(), 2);
                    assert!(staged.roots[0].join("empty").is_dir());
                    assert_eq!(
                        std::fs::read(staged.roots[0].join("binary.dat")).unwrap(),
                        [0, 255, 13, 10]
                    );
                    assert_eq!(std::fs::metadata(&staged.roots[1]).unwrap().len(), 0);
                }
            }
            assert!(
                backend
                    .write(backend.version().wrapping_sub(1), &content)
                    .unwrap()
                    .is_none()
            );
        }
    }
}
