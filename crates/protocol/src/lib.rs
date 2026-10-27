//! Bounded control messages. Video payloads will use a separate connection.
use serde::{Deserialize, Serialize};
use std::io::{self, ErrorKind};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u16 = 1;
pub const MAX_CONTROL_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Hello { version: u16 },
    Welcome { version: u16 },
    Ping { sequence: u64 },
    Pong { sequence: u64 },
    Error { code: ErrorCode },
    Goodbye,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    UnexpectedMessage,
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
}
