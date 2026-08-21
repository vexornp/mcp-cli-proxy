use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, bytes: &[u8]) -> io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::other("frame too large"))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(bytes).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::other("frame too large"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trips_payload() {
        let (mut tx, mut rx) = duplex(8192);
        let payload = b"{\"hello\":\"world\"}";
        write_frame(&mut tx, payload).await.unwrap();
        let got = read_frame(&mut rx).await.unwrap();
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn empty_payload_round_trips() {
        let (mut tx, mut rx) = duplex(8192);
        write_frame(&mut tx, b"").await.unwrap();
        let got = read_frame(&mut rx).await.unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn length_prefix_is_be_u32() {
        let (mut tx, mut rx) = duplex(8192);
        write_frame(&mut tx, b"abc").await.unwrap();
        let mut prefix = [0u8; 4];
        rx.read_exact(&mut prefix).await.unwrap();
        assert_eq!(u32::from_be_bytes(prefix), 3);
        let mut body = [0u8; 3];
        rx.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"abc");
    }

    #[tokio::test]
    async fn eof_before_frame_returns_unexpected_eof() {
        let (_tx, mut rx) = duplex(8192);
        drop(_tx);
        let err = read_frame(&mut rx).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
