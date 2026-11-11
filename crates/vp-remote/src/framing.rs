//! Bounded newline framing.
//!
//! `read_until(b'\n')` would allocate an unbounded line before the caller can
//! reject it. These helpers inspect the buffered slice and fail as soon as the
//! configured maximum is exceeded, which keeps a compromised or broken peer
//! from exhausting helper or daemon memory.

use std::io;

use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::{MAX_FRAME_BYTES, RemoteError, RemoteErrorCode};

pub async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_frame_bytes: usize,
) -> io::Result<Option<Vec<u8>>> {
    let max_frame_bytes = max_frame_bytes.clamp(1024, MAX_FRAME_BYTES as usize);
    let mut frame = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            if frame.is_empty() {
                return Ok(None);
            }
            return Ok(Some(frame));
        }
        if let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
            if frame.len() + index > max_frame_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("frame exceeds the {max_frame_bytes}-byte limit"),
                ));
            }
            frame.extend_from_slice(&buffer[..index]);
            reader.consume(index + 1);
            if frame.iter().all(u8::is_ascii_whitespace) {
                frame.clear();
                continue;
            }
            return Ok(Some(frame));
        }
        if frame.len() + buffer.len() > max_frame_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame exceeds the {max_frame_bytes}-byte limit"),
            ));
        }
        frame.extend_from_slice(buffer);
        let consumed = buffer.len();
        reader.consume(consumed);
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
    max_frame_bytes: usize,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cannot encode JSON-RPC frame: {error}"),
        )
    })?;
    if bytes.len() + 1 > max_frame_bytes {
        let error = RemoteError::new(
            RemoteErrorCode::OutputLimit,
            format!(
                "response frame is {} bytes; the limit is {}",
                bytes.len() + 1,
                max_frame_bytes
            ),
        );
        bytes = serde_json::to_vec(&vp_protocol::rpc::RpcResponse::error(
            None,
            error.to_rpc_error(),
        ))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        if bytes.len() + 1 > max_frame_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "even the error frame exceeds the frame limit",
            ));
        }
    }
    writer.write_all(&bytes).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;

    #[tokio::test]
    async fn round_trips_a_frame() {
        let (mut client, server) = duplex(4096);
        tokio::spawn(async move {
            write_frame(&mut client, &serde_json::json!({"ok": true}), 4096)
                .await
                .unwrap();
            write_frame(&mut client, &serde_json::json!({"ok": 2}), 4096)
                .await
                .unwrap();
        });
        let mut server = tokio::io::BufReader::new(server);
        let first = read_frame(&mut server, 4096).await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&first).unwrap(),
            serde_json::json!({"ok": true})
        );
        let second = read_frame(&mut server, 4096).await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&second).unwrap(),
            serde_json::json!({"ok": 2})
        );
        assert!(read_frame(&mut server, 4096).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_oversized_frames_before_allocating_them() {
        let (mut client, server) = duplex(8192);
        tokio::spawn(async move {
            let _ = client.write_all(&vec![b'x'; 5000]).await;
            let _ = client.write_all(b"\n").await;
        });
        let mut server = tokio::io::BufReader::new(server);
        let error = read_frame(&mut server, 1024).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn skips_blank_lines() {
        let (mut client, server) = duplex(4096);
        tokio::spawn(async move {
            let _ = client.write_all(b"\n\n{\"ping\":1}\n").await;
        });
        let mut server = tokio::io::BufReader::new(server);
        let frame = read_frame(&mut server, 4096).await.unwrap().unwrap();
        assert_eq!(frame, br#"{"ping":1}"#);
    }
}

#[cfg(test)]
mod final_frame_tests {
    use super::*;

    #[tokio::test]
    async fn accepts_a_final_frame_without_a_newline() {
        let (mut client, server) = tokio::io::duplex(64);
        tokio::spawn(async move {
            let _ = client.write_all(br#"{"final":true}"#).await;
            let _ = client.shutdown().await;
        });
        let mut server = tokio::io::BufReader::new(server);
        let frame = read_frame(&mut server, 4096).await.unwrap().unwrap();
        assert_eq!(frame, br#"{"final":true}"#);
        assert!(read_frame(&mut server, 4096).await.unwrap().is_none());
    }
}
