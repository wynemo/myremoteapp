//! A diagnostic session; no capture or input capabilities are advertised yet.
use anyhow::{Result, bail};
use remote_protocol::{ErrorCode, Message, VERSION, read_message, write_message};
use std::time::{Duration, Instant};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

pub const IO_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn serve<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<()> {
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
}
