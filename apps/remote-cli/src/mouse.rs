use remote_protocol::{Message, MouseAction, MouseButton, MouseEvent};

#[derive(Default)]
pub struct MouseTracker {
    physical: [bool; 3],
    held: [bool; 3],
    point: Option<(u16, u16)>,
}

/// Match minifb's integer aspect-fit viewport; letterbox pixels are not input.
fn normalize(
    position: (f32, f32),
    window: (usize, usize),
    frame: (usize, usize),
) -> Option<(u16, u16)> {
    let (ww, wh) = window;
    let (fw, fh) = frame;
    if ww == 0
        || wh == 0
        || fw == 0
        || fh == 0
        || !position.0.is_finite()
        || !position.1.is_finite()
    {
        return None;
    }
    let aspect = fw as f32 / fh as f32;
    let (w, h) = if aspect > ww as f32 / wh as f32 {
        (ww, (ww as f32 / aspect) as usize)
    } else {
        ((wh as f32 * aspect) as usize, wh)
    };
    if w == 0 || h == 0 {
        return None;
    }
    let x = f64::from(position.0) - ((ww - w) / 2) as f64;
    let y = f64::from(position.1) - ((wh - h) / 2) as f64;
    if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 {
        return None;
    }
    Some((
        (((x + 0.5) / w as f64) * 65535.0).round().min(65534.0) as u16,
        (((y + 0.5) / h as f64) * 65535.0).round().min(65534.0) as u16,
    ))
}

pub struct MouseSample {
    pub position: Option<(f32, f32)>,
    pub window: (usize, usize),
    pub frame: (usize, usize),
    pub sequence: u64,
    pub buttons: [bool; 3],
    pub scroll: Option<(f32, f32)>,
    pub active: bool,
}

impl MouseTracker {
    pub fn update(&mut self, sample: MouseSample) -> Vec<Message> {
        let mut output = Vec::new();
        let previous = self.physical;
        self.physical = sample.buttons;
        let point = sample
            .position
            .and_then(|p| normalize(p, sample.window, sample.frame))
            .filter(|_| sample.active);
        let Some((x, y)) = point else {
            if self.held.iter().any(|held| *held) {
                output.push(Message::ReleaseInput);
            }
            self.held = [false; 3];
            self.point = None;
            return output;
        };
        let event = |kind| Message::Mouse {
            event: MouseEvent {
                frame_sequence: sample.sequence,
                x,
                y,
                kind,
            },
        };
        if self.point != Some((x, y)) {
            output.push(event(MouseAction::Move));
            self.point = Some((x, y));
        }
        for (index, button) in [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
            .into_iter()
            .enumerate()
        {
            if !sample.buttons[index] && self.held[index] {
                output.push(event(MouseAction::Button {
                    button,
                    down: false,
                }));
                self.held[index] = false;
            } else if sample.buttons[index] && !previous[index] {
                output.push(event(MouseAction::Button { button, down: true }));
                self.held[index] = true;
            }
        }
        if let Some((dx, dy)) = sample.scroll {
            let dx = (dx * 3.0).round().clamp(-50.0, 50.0) as i16;
            let dy = (dy * 3.0).round().clamp(-50.0, 50.0) as i16;
            if dx != 0 || dy != 0 {
                output.push(event(MouseAction::Scroll { dx, dy }));
            }
        }
        output
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod connection {
    use super::*;
    use anyhow::{Result, bail};
    use remote_protocol::{read_message, write_message};
    use tokio::{
        io::{AsyncRead, AsyncWrite, AsyncWriteExt},
        sync::{mpsc, watch},
        task::JoinHandle,
        time::{Duration, timeout},
    };

    #[derive(Clone, PartialEq)]
    pub enum Status {
        Ready,
        Notice(String),
        Failed(String),
    }
    pub struct ControlClient {
        sender: mpsc::Sender<Message>,
        status: watch::Receiver<Status>,
        writer: Option<JoinHandle<()>>,
        reader: JoinHandle<()>,
    }
    impl ControlClient {
        pub async fn start<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
            mut stream: S,
            window: u32,
        ) -> Result<Self> {
            remote_session::start_control(&mut stream, window).await?;
            let (mut read, mut write) = tokio::io::split(stream);
            let (sender, mut input) = mpsc::channel(64);
            let (status_tx, status) = watch::channel(Status::Ready);
            let failure = status_tx.clone();
            let writer = tokio::spawn(async move {
                let result: Result<()> = async {
                    let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
                    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        let message = tokio::select! {
                            message=input.recv()=>match message {Some(message)=>message,None=>break},
                            _=heartbeat.tick()=>Message::Ping { sequence:0 },
                        };
                        timeout(remote_session::IO_TIMEOUT,write_message(&mut write,&message)).await??;
                        if message==Message::Goodbye { write.shutdown().await?; break; }
                    }
                    Ok(())
                }.await;
                if let Err(error) = result {
                    failure.send_replace(Status::Failed(format!("鼠标连接已断开：{error:#}")));
                }
            });
            let reader = tokio::spawn(async move {
                loop {
                    match timeout(remote_session::IO_TIMEOUT, read_message(&mut read)).await {
                        Ok(Ok(Message::Pong { .. })) => {
                            status_tx.send_replace(Status::Ready);
                        }
                        Ok(Ok(Message::InputError { message })) => {
                            status_tx.send_replace(Status::Notice(message));
                        }
                        result => {
                            status_tx.send_replace(Status::Failed(format!(
                                "鼠标连接已断开，请重新连接：{result:?}"
                            )));
                            break;
                        }
                    }
                }
            });
            Ok(Self {
                sender,
                status,
                writer: Some(writer),
                reader,
            })
        }
        pub fn status(&self) -> Status {
            self.status.borrow().clone()
        }
        pub fn send(&self, message: Message) -> Result<()> {
            match self.sender.try_send(message) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Full(Message::Mouse {
                    event:
                        MouseEvent {
                            kind: MouseAction::Move,
                            ..
                        },
                })) => Ok(()),
                Err(_) => bail!("鼠标连接拥堵或断开，请重新连接"),
            }
        }
        pub async fn finish(mut self) {
            let _ = timeout(Duration::from_secs(2), async {
                self.sender.send(Message::ReleaseInput).await?;
                self.sender.send(Message::Goodbye).await
            })
            .await;
            if let Some(mut writer) = self.writer.take()
                && timeout(Duration::from_secs(2), &mut writer).await.is_err()
            {
                writer.abort();
            }
        }
    }
    impl Drop for ControlClient {
        fn drop(&mut self) {
            if let Some(writer) = &self.writer {
                writer.abort();
            }
            self.reader.abort();
        }
    }
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use connection::{ControlClient, Status};

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> MouseSample {
        MouseSample {
            position: Some((500.0, 500.0)),
            window: (1000, 1000),
            frame: (1280, 720),
            sequence: 4,
            buttons: [false; 3],
            scroll: None,
            active: true,
        }
    }
    #[test]
    fn rejects_letterbox_and_maps_resized_center() {
        assert!(normalize((500.0, 50.0), (1000, 1000), (1280, 720)).is_none());
        let (x, y) = normalize((500.0, 500.0), (1000, 1000), (1280, 720)).unwrap();
        assert!((i32::from(x) - 32768).abs() < 100 && (i32::from(y) - 32768).abs() < 100);
    }
    #[test]
    fn release_on_focus_loss_and_no_press_when_reentering_held() {
        let mut tracker = MouseTracker::default();
        let mut current = sample();
        current.buttons[0] = true;
        assert!(tracker.update(current).iter().any(|m| matches!(
            m,
            Message::Mouse {
                event: MouseEvent {
                    kind: MouseAction::Button { down: true, .. },
                    ..
                }
            }
        )));
        let mut current = sample();
        current.active = false;
        current.buttons[0] = true;
        assert_eq!(tracker.update(current), vec![Message::ReleaseInput]);
        let mut current = sample();
        current.buttons[0] = true;
        assert!(!tracker.update(current).iter().any(|m| matches!(
            m,
            Message::Mouse {
                event: MouseEvent {
                    kind: MouseAction::Button { down: true, .. },
                    ..
                }
            }
        )));
    }
    #[test]
    fn forwards_release_and_scroll_once() {
        let mut tracker = MouseTracker::default();
        let mut current = sample();
        current.buttons[0] = true;
        tracker.update(current);
        let mut current = sample();
        current.scroll = Some((0.0, 1.0));
        let events = tracker.update(current);
        assert!(events.iter().any(|m| matches!(
            m,
            Message::Mouse {
                event: MouseEvent {
                    kind: MouseAction::Button { down: false, .. },
                    ..
                }
            }
        )));
        assert!(events.iter().any(|m| matches!(
            m,
            Message::Mouse {
                event: MouseEvent {
                    kind: MouseAction::Scroll { dy: 3, .. },
                    ..
                }
            }
        )));
        assert!(tracker.update(sample()).is_empty());
    }
}
