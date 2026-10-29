//! Bounded control messages and the initial JPEG preview wire format.
use serde::{Deserialize, Serialize};
use std::io::{self, ErrorKind};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u16 = 5;
pub const MAX_PASSWORD_BYTES: usize = 4096;
pub const MAX_CONTROL_BYTES: usize = 64 * 1024;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_FRAME_WIDTH: u32 = 2560;
pub const MAX_FRAME_HEIGHT: u32 = 2560;
pub const MAX_FRAME_PIXELS: usize = 2560 * 1440;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowInfo {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
    pub jpeg: Vec<u8>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MouseAction {
    Move,
    Button { button: MouseButton, down: bool },
    Scroll { dx: i16, dy: i16 },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MouseEvent {
    pub frame_sequence: u64,
    pub x: u16,
    pub y: u16,
    pub kind: MouseAction,
}

/// Named keys; text is interpreted by the host's keyboard layout and IME.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyCode {
    Key0,
    Key1,
    Key2,
    Key3,
    Key4,
    Key5,
    Key6,
    Key7,
    Key8,
    Key9,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    Down,
    Left,
    Right,
    Up,
    Apostrophe,
    Backquote,
    Backslash,
    Comma,
    Equal,
    LeftBracket,
    Minus,
    Period,
    RightBracket,
    Semicolon,
    Slash,
    Backspace,
    Delete,
    End,
    Enter,
    Escape,
    Home,
    Insert,
    Menu,
    PageDown,
    PageUp,
    Pause,
    Space,
    Tab,
    NumLock,
    CapsLock,
    ScrollLock,
    LeftShift,
    RightShift,
    LeftCtrl,
    RightCtrl,
    NumPad0,
    NumPad1,
    NumPad2,
    NumPad3,
    NumPad4,
    NumPad5,
    NumPad6,
    NumPad7,
    NumPad8,
    NumPad9,
    NumPadDot,
    NumPadSlash,
    NumPadAsterisk,
    NumPadMinus,
    NumPadPlus,
    NumPadEnter,
    LeftAlt,
    RightAlt,
    LeftSuper,
    RightSuper,
}
impl KeyCode {
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            Self::LeftShift
                | Self::RightShift
                | Self::LeftCtrl
                | Self::RightCtrl
                | Self::LeftAlt
                | Self::RightAlt
                | Self::LeftSuper
                | Self::RightSuper
        )
    }
    pub fn repeatable(self) -> bool {
        !self.is_modifier() && !matches!(self, Self::CapsLock | Self::NumLock | Self::ScrollLock)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyEvent {
    pub frame_sequence: u64,
    pub key: KeyCode,
    pub down: bool,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Hello {
        version: u16,
    },
    Welcome {
        version: u16,
    },
    Ping {
        sequence: u64,
    },
    Pong {
        sequence: u64,
    },
    Error {
        code: ErrorCode,
    },
    Goodbye,
    StartPreview,
    PreviewStarted {
        window: WindowInfo,
    },
    StartControl {
        window_id: u32,
    },
    ControlReady,
    Mouse {
        event: MouseEvent,
    },
    Key {
        event: KeyEvent,
    },
    ReleaseMouse,
    ReleaseKeyboard,
    ReleaseInput,
    InputError {
        message: String,
    },
    GetStatus,
    Status {
        locked: bool,
        unlock_available: bool,
    },
    /// The password follows as a separate bounded binary record, never JSON.
    Unlock,
    UnlockResult {
        outcome: UnlockOutcome,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockOutcome {
    Unlocked,
    Rejected,
    Unavailable,
    Busy,
    WindowUnavailable,
}

pub async fn write_secret<W: AsyncWrite + Unpin>(
    writer: &mut W,
    password: &[u8],
) -> io::Result<()> {
    if password.is_empty()
        || password.len() > MAX_PASSWORD_BYTES
        || password.contains(&0)
        || std::str::from_utf8(password).is_err()
    {
        return Err(invalid_data("invalid password record"));
    }
    writer.write_u32(password.len() as u32).await?;
    writer.write_all(password).await?;
    writer.flush().await
}

pub async fn read_secret<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<zeroize::Zeroizing<Vec<u8>>> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_PASSWORD_BYTES {
        return Err(invalid_data("invalid password record"));
    }
    let mut bytes = zeroize::Zeroizing::new(vec![0; length]);
    reader.read_exact(&mut bytes).await?;
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Err(invalid_data("invalid password record"));
    }
    Ok(bytes)
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    UnexpectedMessage,
    NoWindowSelected,
    ControlUnavailable,
    ControlBusy,
}

pub fn validate_frame_size(width: u32, height: u32) -> io::Result<()> {
    if width == 0
        || height == 0
        || width > MAX_FRAME_WIDTH
        || height > MAX_FRAME_HEIGHT
        || u64::from(width) * u64::from(height) > MAX_FRAME_PIXELS as u64
    {
        return Err(invalid_data("invalid preview dimensions"));
    }
    Ok(())
}

/// Preview records: length (0 means capture stopped), width, height, sequence,
/// then exactly `length` JPEG bytes. All integers are big-endian.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &PreviewFrame,
) -> io::Result<()> {
    validate_frame_size(frame.width, frame.height)?;
    if frame.jpeg.is_empty() || frame.jpeg.len() > MAX_FRAME_BYTES {
        return Err(invalid_data("invalid preview frame length"));
    }
    writer.write_u32(frame.jpeg.len() as u32).await?;
    writer.write_u32(frame.width).await?;
    writer.write_u32(frame.height).await?;
    writer.write_u64(frame.sequence).await?;
    writer.write_all(&frame.jpeg).await?;
    writer.flush().await
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<PreviewFrame>> {
    let length = reader.read_u32().await? as usize;
    if length == 0 {
        return Ok(None);
    }
    if length > MAX_FRAME_BYTES {
        return Err(invalid_data("preview frame too large"));
    }
    let width = reader.read_u32().await?;
    let height = reader.read_u32().await?;
    validate_frame_size(width, height)?;
    let sequence = reader.read_u64().await?;
    let mut jpeg = vec![0; length];
    reader.read_exact(&mut jpeg).await?;
    Ok(Some(PreviewFrame {
        width,
        height,
        sequence,
        jpeg,
    }))
}

pub async fn end_preview<W: AsyncWrite + Unpin>(writer: &mut W) -> io::Result<()> {
    writer.write_u32(0).await?;
    writer.flush().await
}

pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> io::Result<()> {
    let payload = serde_json::to_vec(message).map_err(invalid_data)?;
    if payload.len() > MAX_CONTROL_BYTES {
        return Err(invalid_data("control message too large"));
    }
    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(&payload).await?;
    writer.flush().await
}

pub async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Message> {
    let len = reader.read_u32().await? as usize;
    if len == 0 || len > MAX_CONTROL_BYTES {
        return Err(invalid_data("invalid control message length"));
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).map_err(invalid_data)
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_dimensions_support_portrait_with_bounded_memory() {
        assert!(validate_frame_size(1440, 2560).is_ok());
        assert!(validate_frame_size(2560, 1440).is_ok());
        assert!(validate_frame_size(2560, 2560).is_err());
        assert!(validate_frame_size(0, 100).is_err());
    }

    #[tokio::test]
    async fn messages_preserve_boundaries() {
        let mut wire = Vec::new();
        write_message(&mut wire, &Message::Hello { version: VERSION })
            .await
            .unwrap();
        write_message(&mut wire, &Message::Ping { sequence: 42 })
            .await
            .unwrap();
        let mut reader = wire.as_slice();
        assert_eq!(
            read_message(&mut reader).await.unwrap(),
            Message::Hello { version: VERSION }
        );
        assert_eq!(
            read_message(&mut reader).await.unwrap(),
            Message::Ping { sequence: 42 }
        );
        assert!(reader.is_empty());
    }

    #[tokio::test]
    async fn key_messages_preserve_modifiers_and_reject_unknown_keys() {
        let events = [
            Message::Key {
                event: KeyEvent {
                    frame_sequence: 9,
                    key: KeyCode::LeftSuper,
                    down: true,
                },
            },
            Message::Key {
                event: KeyEvent {
                    frame_sequence: 9,
                    key: KeyCode::A,
                    down: true,
                },
            },
            Message::Key {
                event: KeyEvent {
                    frame_sequence: 9,
                    key: KeyCode::A,
                    down: false,
                },
            },
            Message::ReleaseKeyboard,
        ];
        let mut wire = Vec::new();
        for message in &events {
            write_message(&mut wire, message).await.unwrap();
        }
        let mut reader = wire.as_slice();
        for message in events {
            assert_eq!(read_message(&mut reader).await.unwrap(), message);
        }
        assert!(reader.is_empty());
        let invalid = br#"{"type":"key","event":{"frame_sequence":1,"key":"arbitrary_scan_code","down":true}}"#;
        let mut wire = (invalid.len() as u32).to_be_bytes().to_vec();
        wire.extend_from_slice(invalid);
        assert_eq!(
            read_message(&mut wire.as_slice()).await.unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn rejects_invalid_lengths_before_reading_payload() {
        for len in [0u32, MAX_CONTROL_BYTES as u32 + 1, u32::MAX] {
            let bytes = len.to_be_bytes();
            assert_eq!(
                read_message(&mut bytes.as_slice())
                    .await
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidData
            );
        }
    }

    #[tokio::test]
    async fn rejects_truncated_payload() {
        let bytes = [0, 0, 0, 8, b'{'];
        assert_eq!(
            read_message(&mut bytes.as_slice())
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn rejects_unknown_message_and_fields() {
        for payload in [
            r#"{"type":"execute"}"#,
            r#"{"type":"ping","sequence":1,"extra":true}"#,
        ] {
            let mut wire = (payload.len() as u32).to_be_bytes().to_vec();
            wire.extend_from_slice(payload.as_bytes());
            assert_eq!(
                read_message(&mut wire.as_slice()).await.unwrap_err().kind(),
                ErrorKind::InvalidData
            );
        }
    }

    #[tokio::test]
    async fn frame_and_stop_preserve_boundaries() {
        let mut wire = Vec::new();
        let frame = PreviewFrame {
            width: 320,
            height: 200,
            sequence: 9,
            jpeg: vec![1, 2, 3],
        };
        write_frame(&mut wire, &frame).await.unwrap();
        end_preview(&mut wire).await.unwrap();
        let mut reader = wire.as_slice();
        let received = read_frame(&mut reader).await.unwrap().unwrap();
        assert_eq!(
            (
                received.width,
                received.height,
                received.sequence,
                received.jpeg
            ),
            (320, 200, 9, vec![1, 2, 3])
        );
        assert!(read_frame(&mut reader).await.unwrap().is_none());
        assert!(reader.is_empty());
    }

    #[tokio::test]
    async fn rejects_oversize_frame_and_bad_dimensions() {
        let length = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
        assert_eq!(
            read_frame(&mut length.as_slice()).await.unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        let mut invalid = Vec::new();
        invalid.extend_from_slice(&1u32.to_be_bytes());
        invalid.extend_from_slice(&(MAX_FRAME_WIDTH + 1).to_be_bytes());
        invalid.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(
            read_frame(&mut invalid.as_slice())
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn secret_record_round_trip_and_invalid_lengths() {
        let mut wire = Vec::new();
        write_secret(&mut wire, b"test-only-secret").await.unwrap();
        let received = read_secret(&mut wire.as_slice()).await.unwrap();
        assert_eq!(received.as_slice(), b"test-only-secret");
        for length in [0u32, MAX_PASSWORD_BYTES as u32 + 1, u32::MAX] {
            assert!(
                read_secret(&mut length.to_be_bytes().as_slice())
                    .await
                    .is_err()
            );
        }
        for invalid in [b"".as_slice(), b"a\0b", b"\xff"] {
            assert!(write_secret(&mut Vec::new(), invalid).await.is_err());
        }
    }
}
