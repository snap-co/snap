//! TLS socket ownership and binary framing only. Commands queue into the same
//! portable dispatch contract as WebSocket; no Store or credential-refresh IO.
use crate::{read_command_sized, tls, write_response};
use snap_transport::{
    Command, binary,
    carrier::{Connection, Dispatch, Physical, Submission},
};
use std::{io, sync::Arc, time::Duration};
use tokio::net::TcpListener;

pub async fn serve<D: Dispatch>(
    listener: TcpListener,
    dispatch: D,
    tls: tls::ServerTls,
) -> io::Result<()> {
    let permits = Arc::new(tokio::sync::Semaphore::new(128));
    let mut peers = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                let dispatch = dispatch.clone();
                let tls = tls.clone();
                peers.spawn(async move {
                    let _permit = permit;
                    if let Ok(socket) = tls.accept(socket).await { let _ = connection(socket, dispatch).await; }
                });
            }
            Some(_) = peers.join_next(), if !peers.is_empty() => {},
        }
    }
}

async fn connection<D: Dispatch>(socket: tls::ServerStream, dispatch: D) -> io::Result<()> {
    let channel = Physical(
        dispatch
            .open(None, binary::LOGICAL_MESSAGE_LIMIT)
            .await
            .map_err(|e| io::Error::other(format!("{e:?}")))?,
    );
    let (mut reader, mut writer) = tokio::io::split(socket);
    let mut flush = tokio::time::interval(Duration::from_millis(2));
    let mut connected = false;
    loop {
        // Keep a partial frame pinned across output ticks. Cancelling and restarting
        // a read would interpret payload bytes as a new header.
        let waiting_connected = connected;
        let reading = async {
            if waiting_connected {
                read_command_sized(&mut reader).await
            } else {
                tokio::time::timeout(Duration::from_secs(30), read_command_sized(&mut reader))
                    .await?
            }
        };
        tokio::pin!(reading);
        loop {
            tokio::select! {
                decoded = &mut reading => {
                    let Some((command, size)) = decoded? else { return Ok(()); };
                    if matches!(command, Command::Connect { .. }) { connected = true; }
                    if !matches!(channel.0.submit(command, size), Ok(Submission::Queued)) { return Ok(()); }
                    break;
                }
                _ = flush.tick() => {
                    let retired = channel.0.retired();
                    while let Some(frame) = channel.0.receive() {
                        write_response(&mut writer, &frame.response, frame.handshake, frame.attachment.as_ref()).await?;
                        if frame.terminal { return Ok(()); }
                    }
                    if retired { return Ok(()); }
                }
            }
        }
    }
}
