//! Framing: 4-byte little-endian length prefix, MessagePack body.
//!
//! MessagePack rather than JSON because a live segment-map frame at 20 Hz across 12 jobs is
//! a lot of numbers, and binary bitmaps do not want base64 (01 §IPC).

use crate::MAX_FRAME;
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("decode: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME} byte limit")]
    TooLarge(u32),
}

/// Reads one frame. Returns `Ok(None)` on a clean EOF at a frame boundary — the peer
/// closing between messages is normal, not an error.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, CodecError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len);
    if len as usize > MAX_FRAME {
        return Err(CodecError::TooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body).await?;
    Ok(Some(rmp_serde::from_slice(&body)?))
}

/// Writes one frame and flushes it. A partially written frame is unrecoverable, so the
/// length prefix and body go out in a single `write_all`.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), CodecError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = rmp_serde::to_vec_named(value)?;
    if body.len() > MAX_FRAME {
        return Err(CodecError::TooLarge(body.len() as u32));
    }
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(&body);
    writer.write_all(&buf).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, RequestEnvelope};

    #[tokio::test]
    async fn frames_round_trip_and_eof_is_not_an_error() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &Command::Ping).await.unwrap();
        write_frame(
            &mut buf,
            &Command::Probe {
                envelope: RequestEnvelope::new("https://example.com/x"),
            },
        )
        .await
        .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(
            read_frame::<_, Command>(&mut cursor).await.unwrap(),
            Some(Command::Ping)
        ));
        assert!(matches!(
            read_frame::<_, Command>(&mut cursor).await.unwrap(),
            Some(Command::Probe { .. })
        ));
        assert!(read_frame::<_, Command>(&mut cursor)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn an_absurd_length_prefix_is_refused_before_allocating() {
        let mut cursor = std::io::Cursor::new(u32::MAX.to_le_bytes().to_vec());
        let err = read_frame::<_, Command>(&mut cursor).await.unwrap_err();
        assert!(matches!(err, CodecError::TooLarge(_)));
    }
}
