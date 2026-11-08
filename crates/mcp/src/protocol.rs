use crate::MAX_FRAME;
use cadpad_core::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn read<R: AsyncBufRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<Option<T>> {
    let mut bytes = Vec::new();
    let count = reader
        .take(MAX_FRAME as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    if count == 0 {
        return Ok(None);
    }
    if count > MAX_FRAME || bytes.last() != Some(&b'\n') {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Local IPC frame is incomplete or exceeds 12 MiB",
        ));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}
pub async fn write<W: AsyncWrite + Unpin, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() >= MAX_FRAME {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Local IPC result exceeds 12 MiB",
        ));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
