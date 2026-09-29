//! Native binary TCP IO. No application dispatch or logical lifetime state lives
//! here. Failed reads/writes have unknown operation outcomes and are never retried.
use snap_transport::{Command, Response, binary};
use std::{io, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};

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
pub async fn read_command<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<Command>> {
    let Some((kind, bytes)) = read_frame(reader).await? else {
        return Ok(None);
    };
    binary::read_command(kind, &bytes)
        .map(Some)
        .map_err(protocol)
}
pub async fn read_response<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<(Response, Option<u64>)> {
    let (kind, bytes) = read_frame(reader).await?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "TCP detached; outstanding outcome is unknown",
        )
    })?;
    binary::read_response(kind, &bytes).map_err(protocol)
}
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &[u8]) -> io::Result<()> {
    tokio::time::timeout(Duration::from_secs(5), writer.write_all(frame)).await??;
    Ok(())
}
pub async fn write_command<W: AsyncWrite + Unpin>(
    writer: &mut W,
    command: &Command,
) -> io::Result<()> {
    write_frame(writer, &binary::command(command).map_err(protocol)?).await
}
pub async fn write_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &Response,
    connect: bool,
    retention_ms: u64,
) -> io::Result<()> {
    write_frame(
        writer,
        &binary::response(response, connect, retention_ms).map_err(protocol)?,
    )
    .await
}

pub struct Client {
    stream: TcpStream,
}
impl Client {
    pub async fn open(addr: std::net::SocketAddr) -> io::Result<Self> {
        // Plaintext credentials are only safe on a protected local channel.
        if !addr.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Plaintext TCP requires loopback; use an SSH tunnel for remote hosts",
            ));
        }
        let stream =
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await??;
        stream.set_nodelay(true)?;
        Ok(Self { stream })
    }
    pub async fn send(&mut self, command: &Command) -> io::Result<()> {
        write_command(&mut self.stream, command).await
    }
    pub async fn receive(&mut self) -> io::Result<(Response, Option<u64>)> {
        read_response(&mut self.stream).await
    }
}
