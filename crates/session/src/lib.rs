//! Authenticated diagnostic, video, and mouse-control sessions.
use anyhow::{Result, bail};
use remote_protocol::{
    ErrorCode, Message, MouseEvent, PreviewFrame, VERSION, WindowInfo, end_preview, read_message,
    write_frame, write_message,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
    time::timeout,
};

pub const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub enum CaptureEvent {
    Frame(Arc<PreviewFrame>),
    Stopped(String),
}

#[derive(Clone)]
pub struct PreviewSource {
    pub window: WindowInfo,
    pub frames: watch::Receiver<Option<CaptureEvent>>,
    pub input: Option<Arc<dyn InputController>>,
}

pub trait InputController: Send + Sync {
    fn acquire(&self) -> Result<Box<dyn InputSession>>;
}

/// Implementors release all held buttons in Drop, including task cancellation.
pub trait InputSession: Send {
    fn apply(&mut self, event: MouseEvent) -> Result<()>;
    fn release_all(&mut self);
}

pub async fn serve<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<()> {
    serve_with_preview(stream, None).await
}

pub async fn serve_with_preview<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    preview: Option<PreviewSource>,
) -> Result<()> {
    let hello = timeout(IO_TIMEOUT, read_message(stream)).await??;
    match hello {
        Message::Hello { version: VERSION } => {
            send(stream, &Message::Welcome { version: VERSION }).await?
        }
        Message::Hello { .. } => {
            send(
                stream,
                &Message::Error {
                    code: ErrorCode::UnsupportedVersion,
                },
            )
            .await?;
            bail!("unsupported protocol version");
        }
        _ => {
            send(
                stream,
                &Message::Error {
                    code: ErrorCode::UnexpectedMessage,
                },
            )
            .await?;
            bail!("expected hello");
        }
    }
    loop {
        match timeout(IO_TIMEOUT, read_message(stream)).await?? {
            Message::Ping { sequence } => send(stream, &Message::Pong { sequence }).await?,
            Message::Goodbye => return Ok(()),
            Message::StartPreview => {
                let Some(source) = preview else {
                    send(
                        stream,
                        &Message::Error {
                            code: ErrorCode::NoWindowSelected,
                        },
                    )
                    .await?;
                    bail!("Mac 未选择共享窗口；请重新运行 serve 并选择窗口");
                };
                return stream_preview(stream, source).await;
            }
            Message::StartControl { window_id } => {
                let Some(source) = preview.filter(|source| source.window.id == window_id) else {
                    send(
                        stream,
                        &Message::Error {
                            code: ErrorCode::NoWindowSelected,
                        },
                    )
                    .await?;
                    bail!("共享窗口已变更，请重新连接");
                };
                let Some(controller) = source.input else {
                    send(
                        stream,
                        &Message::Error {
                            code: ErrorCode::ControlUnavailable,
                        },
                    )
                    .await?;
                    bail!("当前共享未启用鼠标控制");
                };
                let mut input = match controller.acquire() {
                    Ok(input) => input,
                    Err(error) => {
                        send(
                            stream,
                            &Message::Error {
                                code: ErrorCode::ControlBusy,
                            },
                        )
                        .await?;
                        return Err(error);
                    }
                };
                send(stream, &Message::ControlReady).await?;
                loop {
                    match timeout(IO_TIMEOUT, read_message(stream)).await?? {
                        Message::Mouse { event } => {
                            if let Err(error) = input.apply(event) {
                                input.release_all();
                                send(
                                    stream,
                                    &Message::InputError {
                                        message: error.to_string(),
                                    },
                                )
                                .await?;
                            }
                        }
                        Message::ReleaseInput => input.release_all(),
                        Message::Ping { sequence } => {
                            send(stream, &Message::Pong { sequence }).await?
                        }
                        Message::Goodbye => return Ok(()),
                        _ => bail!("unexpected control message"),
                    }
                }
            }
            _ => {
                send(
                    stream,
                    &Message::Error {
                        code: ErrorCode::UnexpectedMessage,
                    },
                )
                .await?;
                bail!("unexpected session message");
            }
        }
    }
}

pub async fn start_control<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    window_id: u32,
) -> Result<()> {
    send(stream, &Message::Hello { version: VERSION }).await?;
    if timeout(IO_TIMEOUT, read_message(stream)).await?? != (Message::Welcome { version: VERSION })
    {
        bail!("鼠标控制协议不兼容，请更新两端程序");
    }
    send(stream, &Message::StartControl { window_id }).await?;
    match timeout(IO_TIMEOUT, read_message(stream)).await?? {
        Message::ControlReady => Ok(()),
        Message::Error {
            code: ErrorCode::ControlBusy,
        } => bail!("已有客户端正在控制该窗口；可加 --view-only 仅观看"),
        Message::Error {
            code: ErrorCode::ControlUnavailable,
        } => bail!("Mac 未启用鼠标控制；请检查辅助功能权限，或加 --view-only 仅观看"),
        _ => bail!("无法开启鼠标控制，请检查 Mac 服务日志"),
    }
}

async fn stream_preview<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    mut source: PreviewSource,
) -> Result<()> {
    send(
        stream,
        &Message::PreviewStarted {
            window: source.window,
        },
    )
    .await?;
    loop {
        let current = source.frames.borrow_and_update().clone();
        match current {
            Some(CaptureEvent::Frame(frame)) => {
                timeout(IO_TIMEOUT, write_frame(stream, &frame)).await??
            }
            Some(CaptureEvent::Stopped(reason)) => {
                timeout(IO_TIMEOUT, end_preview(stream)).await??;
                bail!("窗口采集已停止：{reason}");
            }
            None => {
                timeout(IO_TIMEOUT, source.frames.changed()).await??;
                continue;
            }
        }
        // Resend the last independent frame as a heartbeat when the window is
        // static. A watch channel retains only the latest frame for slow peers.
        match timeout(Duration::from_secs(1), source.frames.changed()).await {
            Ok(Ok(())) | Err(_) => {}
            Ok(Err(_)) => {
                timeout(IO_TIMEOUT, end_preview(stream)).await??;
                return Ok(());
            }
        }
    }
}

pub async fn start_preview<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
) -> Result<WindowInfo> {
    send(stream, &Message::Hello { version: VERSION }).await?;
    if timeout(IO_TIMEOUT, read_message(stream)).await?? != (Message::Welcome { version: VERSION })
    {
        bail!("服务端协议不兼容，请更新两端程序");
    }
    send(stream, &Message::StartPreview).await?;
    match timeout(IO_TIMEOUT, read_message(stream)).await?? {
        Message::PreviewStarted { window } => Ok(window),
        Message::Error {
            code: ErrorCode::NoWindowSelected,
        } => bail!("Mac 未选择共享窗口；请在 Mac 重启 serve 并选择窗口"),
        _ => bail!("服务端未开启窗口预览，请更新 Mac 端并选择窗口"),
    }
}

pub async fn probe<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<Duration> {
    send(stream, &Message::Hello { version: VERSION }).await?;
    if timeout(IO_TIMEOUT, read_message(stream)).await?? != (Message::Welcome { version: VERSION })
    {
        bail!("host rejected protocol handshake");
    }
    let start = Instant::now();
    send(stream, &Message::Ping { sequence: 1 }).await?;
    if timeout(IO_TIMEOUT, read_message(stream)).await?? != (Message::Pong { sequence: 1 }) {
        bail!("unexpected ping response");
    }
    let elapsed = start.elapsed();
    send(stream, &Message::Goodbye).await?;
    Ok(elapsed)
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, message: &Message) -> Result<()> {
    timeout(IO_TIMEOUT, write_message(stream, message)).await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn diagnostic_session_completes() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let host = tokio::spawn(async move { serve(&mut server).await });
        probe(&mut client).await.unwrap();
        host.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rejects_version_mismatch() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let host = tokio::spawn(async move { serve(&mut server).await });
        write_message(
            &mut client,
            &Message::Hello {
                version: VERSION + 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            read_message(&mut client).await.unwrap(),
            Message::Error {
                code: ErrorCode::UnsupportedVersion
            }
        );
        assert!(host.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn input_before_handshake_is_rejected() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let host = tokio::spawn(async move { serve(&mut server).await });
        write_message(&mut client, &Message::Ping { sequence: 1 })
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut client).await.unwrap(),
            Message::Error {
                code: ErrorCode::UnexpectedMessage
            }
        );
        assert!(host.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn preview_delivers_frame_then_signals_capture_stop() {
        let (sender, frames) = watch::channel(Some(CaptureEvent::Frame(Arc::new(PreviewFrame {
            width: 4,
            height: 4,
            sequence: 1,
            jpeg: vec![7],
        }))));
        let source = PreviewSource {
            window: WindowInfo {
                id: 5,
                app: "Test".into(),
                title: "Only this window".into(),
                width: 4,
                height: 4,
            },
            frames,
            input: None,
        };
        let (mut client, mut server) = tokio::io::duplex(1024);
        let host = tokio::spawn(async move { serve_with_preview(&mut server, Some(source)).await });
        assert_eq!(start_preview(&mut client).await.unwrap().id, 5);
        assert_eq!(
            remote_protocol::read_frame(&mut client)
                .await
                .unwrap()
                .unwrap()
                .jpeg,
            vec![7]
        );
        sender.send_replace(Some(CaptureEvent::Stopped("window closed".into())));
        assert!(
            remote_protocol::read_frame(&mut client)
                .await
                .unwrap()
                .is_none()
        );
        assert!(host.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn preview_without_selected_window_is_rejected() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let host = tokio::spawn(async move { serve(&mut server).await });
        assert!(start_preview(&mut client).await.is_err());
        assert!(host.await.unwrap().is_err());
    }

    struct TestController(Arc<std::sync::atomic::AtomicBool>);
    struct TestInput(Arc<std::sync::atomic::AtomicBool>);
    impl InputController for TestController {
        fn acquire(&self) -> Result<Box<dyn InputSession>> {
            Ok(Box::new(TestInput(self.0.clone())))
        }
    }
    impl InputSession for TestInput {
        fn apply(&mut self, event: MouseEvent) -> Result<()> {
            use remote_protocol::{MouseAction, MouseButton};
            if let MouseAction::Button {
                button: MouseButton::Left,
                down,
            } = event.kind
            {
                self.0.store(down, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
        fn release_all(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl Drop for TestInput {
        fn drop(&mut self) {
            self.release_all();
        }
    }

    #[tokio::test]
    async fn cancelled_control_session_releases_pressed_button() {
        use remote_protocol::{MouseAction, MouseButton};
        let pressed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (_sender, frames) = watch::channel(None);
        let source = PreviewSource {
            window: WindowInfo {
                id: 5,
                app: "Test".into(),
                title: "Test".into(),
                width: 4,
                height: 4,
            },
            frames,
            input: Some(Arc::new(TestController(pressed.clone()))),
        };
        let (mut client, mut server) = tokio::io::duplex(2048);
        let host = tokio::spawn(async move { serve_with_preview(&mut server, Some(source)).await });
        start_control(&mut client, 5).await.unwrap();
        write_message(
            &mut client,
            &Message::Mouse {
                event: MouseEvent {
                    frame_sequence: 0,
                    x: 32768,
                    y: 32768,
                    kind: MouseAction::Button {
                        button: MouseButton::Left,
                        down: true,
                    },
                },
            },
        )
        .await
        .unwrap();
        write_message(&mut client, &Message::Ping { sequence: 9 })
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut client).await.unwrap(),
            Message::Pong { sequence: 9 }
        );
        assert!(pressed.load(std::sync::atomic::Ordering::SeqCst));
        host.abort();
        let _ = host.await;
        assert!(!pressed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn control_is_unavailable_in_view_only_session() {
        let (_sender, frames) = watch::channel(None);
        let source = PreviewSource {
            window: WindowInfo {
                id: 5,
                app: "Test".into(),
                title: "Test".into(),
                width: 4,
                height: 4,
            },
            frames,
            input: None,
        };
        let (mut client, mut server) = tokio::io::duplex(2048);
        let host = tokio::spawn(async move { serve_with_preview(&mut server, Some(source)).await });
        assert!(
            start_control(&mut client, 5)
                .await
                .unwrap_err()
                .to_string()
                .contains("辅助功能")
        );
        assert!(host.await.unwrap().is_err());
    }
}
