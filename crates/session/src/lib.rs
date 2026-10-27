//! A diagnostic session; no capture or input capabilities are advertised yet.
use anyhow::{Result, bail};
use remote_protocol::{
    ErrorCode, Message, PreviewFrame, VERSION, WindowInfo, end_preview, read_message, write_frame,
    write_message,
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
}
