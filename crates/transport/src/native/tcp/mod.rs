//! Native binary TCP IO. No application dispatch or logical lifetime state lives
//! here. Failed reads/writes have unknown operation outcomes and are never retried.
use snap_transport::{Command, Response, binary};
use std::{io, time::Duration};
#[cfg(feature = "native-server")]
mod server;
pub mod tls;
#[cfg(feature = "native-server")]
pub use server::serve;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

fn protocol(error: snap_transport::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}"))
}

/// Cancellation-safe only when the read future remains pinned until completion.
/// The first byte may wait indefinitely; the remainder has a five-second deadline.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut header = [0; binary::HEADER_LEN];
    if reader.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        reader.read_exact(&mut header[1..]).await?;
        let (kind, size) = binary::header(&header).map_err(protocol)?;
        let mut body = vec![0; size];
        reader.read_exact(&mut body).await?;
        Ok(Some((kind, body)))
    })
    .await?
}
#[cfg(feature = "native-server")]
pub async fn read_command<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<Command>> {
    Ok(read_command_sized(reader)
        .await?
        .map(|(command, _)| command))
}
/// Wire payload size lets hosts bound aggregate queued input as well as each
/// logical message, without reserializing credentials or application data.
#[cfg(feature = "native-server")]
pub async fn read_command_sized<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<Option<(Command, usize)>> {
    let Some((kind, bytes)) = read_payload(reader).await? else {
        return Ok(None);
    };
    binary::read_command(kind, &bytes)
        .map(|command| Some((command, bytes.len())))
        .map_err(protocol)
}
pub async fn read_response<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<(Response, Option<binary::AttachmentInfo>)> {
    let (kind, bytes) = read_payload(reader).await?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "TCP detached; outstanding outcome is unknown",
        )
    })?;
    binary::read_response(kind, &bytes).map_err(protocol)
}
/// Reassembly preserves stream order and forbids interleaving. Grow only by bytes
/// actually received, not by an untrusted declared total. The entire continuation
/// sequence has a five-second deadline and a 16 MiB logical limit.
pub async fn read_payload<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<Option<(u8, Vec<u8>)>> {
    let Some((kind, bytes)) = read_frame(reader).await? else {
        return Ok(None);
    };
    if kind != binary::SEGMENT {
        return Ok(Some((kind, bytes)));
    }
    let (total, offset, data) = binary::segment(&bytes).map_err(protocol)?;
    if offset != 0 {
        return Err(protocol(snap_transport::Error::Protocol));
    }
    let mut assembled = data.to_vec();
    tokio::time::timeout(Duration::from_secs(5), async {
        while assembled.len() < total {
            let (kind, payload) = read_frame(reader).await?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "Truncated segmented message")
            })?;
            if kind != binary::SEGMENT {
                return Err(protocol(snap_transport::Error::Protocol));
            }
            let (next_total, offset, data) = binary::segment(&payload).map_err(protocol)?;
            if next_total != total || offset != assembled.len() {
                return Err(protocol(snap_transport::Error::Protocol));
            }
            assembled.extend_from_slice(data);
        }
        Ok(Some((binary::MESSAGE, assembled)))
    })
    .await?
}
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &[u8]) -> io::Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        writer.write_all(frame).await?;
        writer.flush().await
    })
    .await??;
    Ok(())
}
pub async fn write_command<W: AsyncWrite + Unpin>(
    writer: &mut W,
    command: &Command,
) -> io::Result<()> {
    write_frame(writer, &binary::command(command).map_err(protocol)?).await
}
#[cfg(feature = "native-server")]
pub async fn write_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &Response,
    connect: bool,
    attachment: Option<&binary::AttachmentInfo>,
) -> io::Result<()> {
    write_frame(
        writer,
        &binary::response(response, connect, attachment).map_err(protocol)?,
    )
    .await
}

pub struct Client {
    stream: tls::ClientStream,
}
impl Client {
    pub async fn open(addr: &str, tls: &tls::ClientTls) -> io::Result<Self> {
        let stream = tls.connect(addr).await?;
        Ok(Self { stream })
    }
    pub async fn send(&mut self, command: &Command) -> io::Result<()> {
        write_command(&mut self.stream, command).await
    }
    pub async fn receive(&mut self) -> io::Result<(Response, Option<binary::AttachmentInfo>)> {
        read_response(&mut self.stream).await
    }
}
