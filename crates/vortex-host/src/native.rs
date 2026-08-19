//! Chrome/Firefox native messaging framing: a 4-byte native-endian length, then UTF-8 JSON.
//!
//! Two limits matter and they are not symmetric. A message *to* the host may be up to 4 GB;
//! a message *from* the host is capped at 1 MB, and exceeding it kills the port with no
//! useful diagnostic on the browser side. That cap is the reason the daemon is not reached
//! through native messaging alone (01 §Why not the obvious alternatives).

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// What the browser will accept from us. Enforced here so the failure is a legible event
/// rather than a dead port.
pub const MAX_TO_BROWSER: usize = 1024 * 1024;
/// What we will accept from the browser. The protocol allows 4 GB; nothing Vortex exchanges
/// comes close, and a length prefix is not a reason to allocate a gigabyte.
pub const MAX_FROM_BROWSER: usize = vortex_proto::MAX_FRAME;

/// Reads one message. `Ok(None)` is the browser closing the port, which is routine.
pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    // Native endianness by specification, not little-endian by convention.
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_FROM_BROWSER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the browser sent a {len} byte message"),
        ));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(Some(body))
}

pub async fn write<W: AsyncWrite + Unpin>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_TO_BROWSER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} bytes is past the 1 MB native messaging limit", body.len()),
        ));
    }
    let mut framed = Vec::with_capacity(4 + body.len());
    framed.extend_from_slice(&(body.len() as u32).to_ne_bytes());
    framed.extend_from_slice(body);
    writer.write_all(&framed).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_round_trip_and_a_closed_port_is_not_an_error() {
        let mut buf = Vec::new();
        write(&mut buf, br#"{"cmd":"ping"}"#).await.unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(
            read(&mut cursor).await.unwrap().as_deref(),
            Some(&br#"{"cmd":"ping"}"#[..])
        );
        assert!(read(&mut cursor).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_oversized_message_is_refused_rather_than_killing_the_port() {
        let mut sink = Vec::new();
        let err = write(&mut sink, &vec![b'x'; MAX_TO_BROWSER + 1])
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(sink.is_empty(), "a refused message must not be half-written");
    }

    #[tokio::test]
    async fn an_absurd_length_prefix_is_refused_before_allocating() {
        let mut cursor = std::io::Cursor::new(u32::MAX.to_ne_bytes().to_vec());
        assert!(read(&mut cursor).await.is_err());
    }
}
