//! Length-prefixed JSON wire helpers.
//!
//! Wire format: 4-byte little-endian u32 body length, then the JSON body.

use serde::{de::DeserializeOwned, Serialize};
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Write a message to `stream` as `[len: u32 LE][json body]`.
pub async fn write_message(stream: &mut UnixStream, msg: &impl Serialize) -> io::Result<()> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&body).await?;
    Ok(())
}

/// Read one message from `stream`, deserialising it as `T`.
pub async fn read_message<T: DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;

    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await?;

    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Open a fresh connection to a plugin socket, send one request, read one response.
///
/// A new connection per call avoids holding a socket across the registry lock and
/// keeps the protocol trivially request/response without multiplexing.
pub async fn call<Req: Serialize, Resp: DeserializeOwned>(
    socket_path: &std::path::Path,
    request: &Req,
) -> io::Result<Resp> {
    let mut stream = UnixStream::connect(socket_path).await?;
    write_message(&mut stream, request).await?;
    read_message(&mut stream).await
}
