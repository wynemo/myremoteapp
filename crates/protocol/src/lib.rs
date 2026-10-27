//! Bounded control messages and the initial JPEG preview wire format.
use serde::{Deserialize, Serialize};
use std::io::{self, ErrorKind};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u16 = 2;
pub const MAX_CONTROL_BYTES: usize = 64 * 1024;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_FRAME_WIDTH: u32 = 1920;
pub const MAX_FRAME_HEIGHT: u32 = 1080;

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

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Hello { version: u16 },
    Welcome { version: u16 },
    Ping { sequence: u64 },
    Pong { sequence: u64 },
    Error { code: ErrorCode },
    Goodbye,
    StartPreview,
    PreviewStarted { window: WindowInfo },
    StartControl { window_id: u32 },
    ControlReady,
    Mouse { event: MouseEvent },
    ReleaseInput,
    InputError { message: String },
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
    if width == 0 || height == 0 || width > MAX_FRAME_WIDTH || height > MAX_FRAME_HEIGHT {
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
}
