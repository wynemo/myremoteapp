//! Authenticated diagnostic, video, and mouse-control sessions.
use anyhow::{Result, bail};
use remote_protocol::{
    ErrorCode, KeyEvent, Message, MouseEvent, PreviewFrame, UnlockOutcome, VERSION, WindowInfo,
    end_preview, read_message, read_secret, write_frame, write_message, write_secret,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
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
    pub demand: PreviewDemand,
}

/// Counts actual video streams, independently of source clones or control sockets.
#[derive(Clone, Default)]
pub struct PreviewDemand(Arc<AtomicUsize>);
impl PreviewDemand {
    pub fn is_active(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }
    fn subscribe(&self) -> PreviewSubscription {
        self.0.fetch_add(1, Ordering::AcqRel);
        PreviewSubscription(self.clone())
    }
}
struct PreviewSubscription(PreviewDemand);
impl Drop for PreviewSubscription {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub trait InputController: Send + Sync {
    fn acquire(&self) -> Result<Box<dyn InputSession>>;
}

/// Implementors release all held buttons in Drop, including task cancellation.
pub trait InputSession: Send {
    fn apply(&mut self, event: MouseEvent) -> Result<()>;
    fn apply_key(&mut self, _event: KeyEvent) -> Result<()> {
        bail!("服务端未启用键盘输入")
    }
    fn release_mouse(&mut self) {
        self.release_all();
    }
    fn release_keyboard(&mut self) {
        self.release_all();
    }
    fn release_all(&mut self);
}

pub trait UnlockHost: Send + Sync {
    fn status(&self) -> (bool, bool);
    fn unlock(&self, password: &[u8], cancelled: &std::sync::atomic::AtomicBool) -> UnlockOutcome;
    fn capture_ready(&self) -> bool;
}

pub async fn serve<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<()> {
    serve_with_preview(stream, None).await
}

pub async fn serve_with_preview<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    preview: Option<PreviewSource>,
) -> Result<()> {
    serve_with_host(stream, preview, None).await
}

pub async fn serve_with_host<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    preview: Option<PreviewSource>,
    unlock: Option<Arc<dyn UnlockHost>>,
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
            Message::GetStatus => {
                let (locked, unlock_available) =
                    unlock.as_ref().map_or((false, false), |host| host.status());
                send(
                    stream,
                    &Message::Status {
                        locked,
                        unlock_available,
                    },
                )
                .await?;
            }
            Message::Unlock => {
                let password = timeout(IO_TIMEOUT, read_secret(stream)).await??;
                let outcome = if let Some(host) = &unlock {
                    let worker = host.clone();
                    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    struct CancelOnDrop(Arc<std::sync::atomic::AtomicBool>);
                    impl Drop for CancelOnDrop {
                        fn drop(&mut self) {
                            self.0.store(true, std::sync::atomic::Ordering::Release);
                        }
                    }
                    let _cancel = CancelOnDrop(cancelled.clone());
                    let mut worker =
                        tokio::task::spawn_blocking(move || worker.unlock(&password, &cancelled));
                    let result = tokio::select! {
                        result=&mut worker=>result,
                        _=read_message(stream)=>return Ok(()),
                    };
                    let mut outcome = result.unwrap_or(UnlockOutcome::Unavailable);
                    if outcome == UnlockOutcome::Unlocked {
                        let deadline = Instant::now() + Duration::from_secs(8);
                        while !host.capture_ready() && Instant::now() < deadline {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        if !host.capture_ready() {
                            outcome = UnlockOutcome::WindowUnavailable;
                        }
                    }
                    outcome
                } else {
                    UnlockOutcome::Unavailable
                };
                send(stream, &Message::UnlockResult { outcome }).await?;
                return Ok(());
            }
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
                    bail!("当前共享未启用键鼠控制");
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
                        Message::Key { event } => {
                            if let Err(error) = input.apply_key(event) {
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
                        Message::ReleaseMouse => input.release_mouse(),
                        Message::ReleaseKeyboard => input.release_keyboard(),
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

pub async fn remote_status<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
) -> Result<(bool, bool)> {
    send(stream, &Message::Hello { version: VERSION }).await?;
    if timeout(IO_TIMEOUT, read_message(stream)).await?? != (Message::Welcome { version: VERSION })
    {
        bail!("服务端协议不兼容，请同时更新 Mac 和 Windows 程序");
    }
    send(stream, &Message::GetStatus).await?;
    let Message::Status {
        locked,
        unlock_available,
    } = timeout(IO_TIMEOUT, read_message(stream)).await??
    else {
        bail!("服务端不支持解锁状态查询，请更新 Mac 端")
    };
    send(stream, &Message::Goodbye).await?;
    Ok((locked, unlock_available))
}

pub async fn unlock_remote<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    password: zeroize::Zeroizing<String>,
) -> Result<()> {
    send(stream, &Message::Hello { version: VERSION }).await?;
    if timeout(IO_TIMEOUT, read_message(stream)).await?? != (Message::Welcome { version: VERSION })
    {
        bail!("解锁协议不兼容，请更新两端程序");
    }
    send(stream, &Message::Unlock).await?;
    timeout(IO_TIMEOUT, write_secret(stream, password.as_bytes())).await??;
    drop(password);
    let result = timeout(Duration::from_secs(25), read_message(stream)).await??;
    match result {
        Message::UnlockResult {
            outcome: UnlockOutcome::Unlocked,
        } => Ok(()),
        Message::UnlockResult {
            outcome: UnlockOutcome::Rejected,
        } => bail!("系统未解锁，密码可能不正确；本次不会自动重试"),
        Message::UnlockResult {
            outcome: UnlockOutcome::Busy,
        } => bail!("解锁正在进行或尝试过于频繁，请稍后重试"),
        Message::UnlockResult {
            outcome: UnlockOutcome::WindowUnavailable,
        } => bail!("Mac 已解锁，但原窗口未能恢复；请在 Mac 重新选择共享窗口"),
        _ => bail!("当前无法解锁，请检查 Mac 的辅助功能权限及系统登录界面"),
    }
}

pub async fn start_control<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    window_id: u32,
) -> Result<()> {
    send(stream, &Message::Hello { version: VERSION }).await?;
    if timeout(IO_TIMEOUT, read_message(stream)).await?? != (Message::Welcome { version: VERSION })
    {
        bail!("键鼠控制协议不兼容，请更新两端程序");
    }
    send(stream, &Message::StartControl { window_id }).await?;
    match timeout(IO_TIMEOUT, read_message(stream)).await?? {
        Message::ControlReady => Ok(()),
        Message::Error {
            code: ErrorCode::ControlBusy,
        } => bail!("已有客户端正在控制该共享画面；可加 --view-only 仅观看"),
        Message::Error {
            code: ErrorCode::ControlUnavailable,
        } => bail!("Mac 未启用键鼠控制；请检查辅助功能权限，或加 --view-only 仅观看"),
        _ => bail!("无法开启键鼠控制，请检查服务端日志"),
    }
}

async fn stream_preview<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    mut source: PreviewSource,
) -> Result<()> {
    let _subscription = source.demand.subscribe();
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
            demand: PreviewDemand::default(),
            input: None,
        };
        let demand = source.demand.clone();
        let (mut client, mut server) = tokio::io::duplex(1024);
        let host = tokio::spawn(async move { serve_with_preview(&mut server, Some(source)).await });
        assert_eq!(start_preview(&mut client).await.unwrap().id, 5);
        assert!(demand.is_active());
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
        assert!(!demand.is_active());
    }

    #[tokio::test]
    async fn preview_pause_resumes_on_the_same_stream_without_end_marker() {
        let frame = |sequence| {
            CaptureEvent::Frame(Arc::new(PreviewFrame {
                width: 4,
                height: 4,
                sequence,
                jpeg: vec![7],
            }))
        };
        let (sender, frames) = watch::channel(Some(frame(1)));
        let source = PreviewSource {
            window: WindowInfo {
                id: 5,
                app: "Windows".into(),
                title: "Desktop".into(),
                width: 4,
                height: 4,
            },
            frames,
            demand: PreviewDemand::default(),
            input: None,
        };
        let demand = source.demand.clone();
        let (mut client, mut server) = tokio::io::duplex(1024);
        let host = tokio::spawn(async move { serve_with_preview(&mut server, Some(source)).await });
        start_preview(&mut client).await.unwrap();
        assert_eq!(
            remote_protocol::read_frame(&mut client)
                .await
                .unwrap()
                .unwrap()
                .sequence,
            1
        );
        sender.send_replace(None);
        assert!(
            timeout(
                Duration::from_millis(50),
                remote_protocol::read_frame(&mut client)
            )
            .await
            .is_err()
        );
        assert!(demand.is_active());
        assert!(!host.is_finished());
        sender.send_replace(Some(frame(2)));
        assert_eq!(
            timeout(IO_TIMEOUT, remote_protocol::read_frame(&mut client))
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .sequence,
            2
        );
        sender.send_replace(Some(CaptureEvent::Stopped(
            "persistent capture failure".into(),
        )));
        assert!(
            remote_protocol::read_frame(&mut client)
                .await
                .unwrap()
                .is_none()
        );
        assert!(host.await.unwrap().is_err());
        assert!(!demand.is_active());
    }

    #[tokio::test]
    async fn capture_demand_tracks_only_video_and_releases_on_cancellation() {
        let (_sender, frames) = watch::channel(None);
        let demand = PreviewDemand::default();
        let source = PreviewSource {
            window: WindowInfo {
                id: 5,
                app: "Test".into(),
                title: "Test".into(),
                width: 4,
                height: 4,
            },
            frames,
            demand: demand.clone(),
            input: None,
        };
        let idle_clone = source.clone();
        assert!(!demand.is_active());
        let (mut client, mut server) = tokio::io::duplex(1024);
        let diagnostic = source.clone();
        let host =
            tokio::spawn(async move { serve_with_preview(&mut server, Some(diagnostic)).await });
        probe(&mut client).await.unwrap();
        host.await.unwrap().unwrap();
        assert!(!demand.is_active());

        let mut hosts = Vec::new();
        let mut clients = Vec::new();
        for _ in 0..2 {
            let (mut client, mut server) = tokio::io::duplex(1024);
            let source = source.clone();
            hosts.push(tokio::spawn(async move {
                serve_with_preview(&mut server, Some(source)).await
            }));
            start_preview(&mut client).await.unwrap();
            clients.push(client);
        }
        assert!(demand.is_active());
        let host = hosts.pop().unwrap();
        host.abort();
        let _ = host.await;
        assert!(demand.is_active(), "remaining viewer still needs capture");
        let host = hosts.pop().unwrap();
        host.abort();
        let _ = host.await;
        assert!(!demand.is_active(), "cancelled viewers must stop capture");
        drop(idle_clone);
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
        fn apply_key(&mut self, event: KeyEvent) -> Result<()> {
            self.0
                .store(event.down, std::sync::atomic::Ordering::SeqCst);
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
            demand: PreviewDemand::default(),
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
    async fn keyboard_routes_down_up_and_releases_on_disconnect() {
        use remote_protocol::KeyCode;
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
            demand: PreviewDemand::default(),
            input: Some(Arc::new(TestController(pressed.clone()))),
        };
        let (mut client, mut server) = tokio::io::duplex(2048);
        let host = tokio::spawn(async move { serve_with_preview(&mut server, Some(source)).await });
        start_control(&mut client, 5).await.unwrap();
        for down in [true, false, true] {
            write_message(
                &mut client,
                &Message::Key {
                    event: KeyEvent {
                        frame_sequence: 0,
                        key: KeyCode::LeftCtrl,
                        down,
                    },
                },
            )
            .await
            .unwrap();
            write_message(&mut client, &Message::Ping { sequence: 1 })
                .await
                .unwrap();
            assert_eq!(
                read_message(&mut client).await.unwrap(),
                Message::Pong { sequence: 1 }
            );
            assert_eq!(pressed.load(std::sync::atomic::Ordering::SeqCst), down);
        }
        write_message(&mut client, &Message::ReleaseKeyboard)
            .await
            .unwrap();
        write_message(&mut client, &Message::Ping { sequence: 2 })
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut client).await.unwrap(),
            Message::Pong { sequence: 2 }
        );
        assert!(!pressed.load(std::sync::atomic::Ordering::SeqCst));
        write_message(
            &mut client,
            &Message::Key {
                event: KeyEvent {
                    frame_sequence: 0,
                    key: KeyCode::LeftShift,
                    down: true,
                },
            },
        )
        .await
        .unwrap();
        write_message(&mut client, &Message::Ping { sequence: 3 })
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut client).await.unwrap(),
            Message::Pong { sequence: 3 }
        );
        drop(client);
        assert!(host.await.unwrap().is_err());
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
            demand: PreviewDemand::default(),
            input: None,
        };
        let (mut client, mut server) = tokio::io::duplex(2048);
        let host = tokio::spawn(async move { serve_with_preview(&mut server, Some(source)).await });
        assert!(
            start_control(&mut client, 5)
                .await
                .unwrap_err()
                .to_string()
                .contains("权限")
        );
        assert!(host.await.unwrap().is_err());
    }

    struct TestUnlock {
        locked: std::sync::atomic::AtomicBool,
        calls: std::sync::atomic::AtomicUsize,
        reject: bool,
    }
    impl UnlockHost for TestUnlock {
        fn status(&self) -> (bool, bool) {
            (self.locked.load(std::sync::atomic::Ordering::SeqCst), true)
        }
        fn capture_ready(&self) -> bool {
            !self.locked.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn unlock(
            &self,
            password: &[u8],
            cancelled: &std::sync::atomic::AtomicBool,
        ) -> UnlockOutcome {
            assert!(!cancelled.load(std::sync::atomic::Ordering::Acquire));
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.reject || password != b"test-only-secret" {
                UnlockOutcome::Rejected
            } else {
                self.locked
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                UnlockOutcome::Unlocked
            }
        }
    }

    #[tokio::test]
    async fn unlock_success_waits_for_recovered_capture_without_resubmitting_password() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        struct RecoveringHost {
            ready: AtomicBool,
            calls: AtomicUsize,
            submitted: tokio::sync::Notify,
        }
        impl UnlockHost for RecoveringHost {
            fn status(&self) -> (bool, bool) {
                (false, true)
            }
            fn capture_ready(&self) -> bool {
                self.ready.load(Ordering::Acquire)
            }
            fn unlock(&self, _: &[u8], _: &AtomicBool) -> UnlockOutcome {
                self.calls.fetch_add(1, Ordering::AcqRel);
                self.submitted.notify_one();
                UnlockOutcome::Unlocked
            }
        }
        let host = Arc::new(RecoveringHost {
            ready: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            submitted: tokio::sync::Notify::new(),
        });
        let (mut client, mut server) = tokio::io::duplex(8192);
        let backend = host.clone();
        let server_task =
            tokio::spawn(async move { serve_with_host(&mut server, None, Some(backend)).await });
        let client_task = tokio::spawn(async move {
            unlock_remote(
                &mut client,
                zeroize::Zeroizing::new("test-only-secret".into()),
            )
            .await
        });
        timeout(IO_TIMEOUT, host.submitted.notified())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !client_task.is_finished(),
            "reported success before capture recovery"
        );
        host.ready.store(true, Ordering::Release);
        timeout(IO_TIMEOUT, client_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server_task.await.unwrap().unwrap();
        assert_eq!(host.calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn status_and_one_shot_password_unlock_use_separate_sessions() {
        let host = Arc::new(TestUnlock {
            locked: std::sync::atomic::AtomicBool::new(true),
            calls: std::sync::atomic::AtomicUsize::new(0),
            reject: false,
        });
        let (mut client, mut server) = tokio::io::duplex(8192);
        let backend = host.clone();
        let task =
            tokio::spawn(async move { serve_with_host(&mut server, None, Some(backend)).await });
        assert_eq!(remote_status(&mut client).await.unwrap(), (true, true));
        task.await.unwrap().unwrap();
        assert_eq!(host.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let (mut client, mut server) = tokio::io::duplex(8192);
        let backend = host.clone();
        let task =
            tokio::spawn(async move { serve_with_host(&mut server, None, Some(backend)).await });
        unlock_remote(
            &mut client,
            zeroize::Zeroizing::new("test-only-secret".into()),
        )
        .await
        .unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(host.status(), (false, true));
        assert_eq!(host.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rejected_password_is_not_retried_or_returned_in_error() {
        let host = Arc::new(TestUnlock {
            locked: std::sync::atomic::AtomicBool::new(true),
            calls: std::sync::atomic::AtomicUsize::new(0),
            reject: true,
        });
        let (mut client, mut server) = tokio::io::duplex(8192);
        let backend = host.clone();
        let task =
            tokio::spawn(async move { serve_with_host(&mut server, None, Some(backend)).await });
        let error = unlock_remote(
            &mut client,
            zeroize::Zeroizing::new("test-only-secret".into()),
        )
        .await
        .unwrap_err();
        assert!(!error.to_string().contains("test-only-secret"));
        assert!(error.to_string().contains("不会自动重试"));
        task.await.unwrap().unwrap();
        assert_eq!(host.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    struct CancellableUnlock {
        entered: tokio::sync::Notify,
        finished: tokio::sync::Notify,
        cancelled: std::sync::atomic::AtomicBool,
    }
    impl UnlockHost for CancellableUnlock {
        fn status(&self) -> (bool, bool) {
            (true, true)
        }
        fn capture_ready(&self) -> bool {
            false
        }
        fn unlock(
            &self,
            _password: &[u8],
            cancelled: &std::sync::atomic::AtomicBool,
        ) -> UnlockOutcome {
            self.entered.notify_one();
            let deadline = Instant::now() + Duration::from_secs(1);
            while !cancelled.load(std::sync::atomic::Ordering::Acquire) && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.cancelled.store(
                cancelled.load(std::sync::atomic::Ordering::Acquire),
                std::sync::atomic::Ordering::Release,
            );
            self.finished.notify_one();
            UnlockOutcome::Unavailable
        }
    }
    #[tokio::test]
    async fn disconnect_cancels_pending_password_submission() {
        let host = Arc::new(CancellableUnlock {
            entered: tokio::sync::Notify::new(),
            finished: tokio::sync::Notify::new(),
            cancelled: std::sync::atomic::AtomicBool::new(false),
        });
        let (mut client, mut server) = tokio::io::duplex(8192);
        let backend = host.clone();
        let task =
            tokio::spawn(async move { serve_with_host(&mut server, None, Some(backend)).await });
        write_message(&mut client, &Message::Hello { version: VERSION })
            .await
            .unwrap();
        read_message(&mut client).await.unwrap();
        write_message(&mut client, &Message::Unlock).await.unwrap();
        write_secret(&mut client, b"test-only-secret")
            .await
            .unwrap();
        host.entered.notified().await;
        drop(client);
        task.await.unwrap().unwrap();
        timeout(Duration::from_secs(1), host.finished.notified())
            .await
            .unwrap();
        assert!(host.cancelled.load(std::sync::atomic::Ordering::Acquire));
    }
}
