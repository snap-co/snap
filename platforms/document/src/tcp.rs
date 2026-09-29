//! Binary TCP feeds the same Host and dispatcher as WebSocket. Output and detach
//! use independent handles so controller IO never blocks ACK/progress delivery.
use crate::web::Shared;
use snap_store::Backend;
use snap_transport::{Command, Event, Response};
use snap_transport_native as io;
use std::{sync::Arc, time::Duration};
use tokio::net::{TcpListener, TcpStream};

pub async fn serve<B: Backend + Send + 'static>(
    listener: TcpListener,
    shared: Arc<Shared<B>>,
) -> std::io::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(std::io::Error::other(
            "Binary TCP requires loopback; terminate remote access through an SSH tunnel",
        ));
    }
    let permits = Arc::new(tokio::sync::Semaphore::new(128));
    let mut peers = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                let shared = shared.clone();
                peers.spawn(async move { let _permit = permit; let _ = connection(socket, shared).await; });
            },
            Some(_) = peers.join_next(), if !peers.is_empty() => {},
        }
    }
}

struct Detach<B: Backend> {
    shared: Arc<Shared<B>>,
    control: crate::CarrierControl,
}
impl<B: Backend> Drop for Detach<B> {
    fn drop(&mut self) {
        self.control.detach(self.shared.now());
    }
}

async fn connection<B: Backend + Send + 'static>(
    socket: TcpStream,
    shared: Arc<Shared<B>>,
) -> std::io::Result<()> {
    socket.set_nodelay(true)?;
    let opening = shared.clone();
    let (peer, output, control, retention, _detach) = tokio::task::spawn_blocking(move || {
        let mut host = opening.host.lock().unwrap();
        let peer = host.open()?;
        Ok::<_, snap_transport::Error>((
            peer,
            host.output(peer)?,
            host.carrier_control(peer)?,
            host.retention_ms(),
            Detach {
                shared: opening.clone(),
                control: host.carrier_control(peer)?,
            },
        ))
    })
    .await
    .map_err(std::io::Error::other)?
    .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    let (mut reader, mut writer) = socket.into_split();
    let mut pending = std::collections::VecDeque::new();
    let mut pending_bytes = 0usize;
    let mut flush = tokio::time::interval(Duration::from_millis(2));
    let mut connected = false;
    let mut submitted = false;
    let mut attachment = None;
    loop {
        // Never cancel a partial frame on a flush tick.
        let waiting_connected = connected;
        let read = async {
            if waiting_connected {
                io::read_command_sized(&mut reader).await
            } else {
                tokio::time::timeout(Duration::from_secs(30), io::read_command_sized(&mut reader))
                    .await?
            }
        };
        tokio::pin!(read);
        loop {
            tokio::select! {
                decoded = &mut read => {
                    let Some((command,size)) = decoded? else { return Ok(()); };
                    if matches!(command, Command::Close | Command::Disconnect) {
                        if matches!(command, Command::Close) { control.close(shared.now()); }
                        return Ok(());
                    }
                    if pending.len() >= 1024 || pending_bytes + size > snap_transport::binary::LOGICAL_MESSAGE_LIMIT { return Ok(()); }
                    if matches!(command, Command::Connect { .. }) { connected = true; }
                    pending_bytes += size;
                    pending.push_back((command,size));
                    break;
                },
                _ = flush.tick() => {
                    let mut failed = None;
                    let mut acquisition = None;
                    if let Ok(mut host) = shared.host.try_lock() {
                        while let Some((command,size)) = pending.pop_front() {
                            pending_bytes -= size;
                            if let Command::Request { invocation, .. } = &command
                                && host.is_preconnection_request(&invocation.operation)
                            {
                                if connected || submitted || !pending.is_empty() { return Ok(()); }
                                acquisition = Some(command);
                                break;
                            }
                            let connect = matches!(command, Command::Connect { .. });
                            submitted = true;
                            if let Err(error) = host.submit(peer, command, shared.now()) { failed = Some((Response::Failed(error), connect)); break; }
                            if connect {
                                attachment = host.attachment_lifetime(peer).ok().map(|lifetime|snap_transport::binary::AttachmentInfo {retention_ms:retention,lifetime});
                            }
                        }
                    }
                    if let Some(Command::Request { invocation, bearer }) = acquisition {
                        // Acquisition never enters retained replay storage. Ordinary
                        // Requests still use Host::submit; sensitive issuance is a
                        // single sanitized exchange on a fresh physical stream.
                        let id = invocation.id;
                        let exchange = shared.clone();
                        let outcome = tokio::task::spawn_blocking(move || exchange.host.lock().unwrap().preconnection_request(invocation, bearer)).await.map_err(std::io::Error::other)?;
                        io::write_response(&mut writer, &Response::Events(vec![Event::Completed { id, outcome }]), false, None).await?;
                        return Ok(());
                    }
                    // Drain queued replies before a subsequent command's immediate
                    // error. Attached/Failed in the Host output belong to CONNECT.
                    while let Some(response) = output.pop_front() {
                        let connect = matches!(response, Response::Attached { .. } | Response::Failed(_));
                        let rejected = matches!(response, Response::Failed(_));
                        io::write_response(&mut writer, &response, connect, attachment.as_ref()).await?;
                        if rejected { return Ok(()); }
                    }
                    if let Some((response, connect)) = failed {
                        io::write_response(&mut writer, &response, connect, attachment.as_ref()).await?;
                        return Ok(());
                    }
                    if shared.host.try_lock().is_ok_and(|host| host.retired(peer)) { return Ok(()); }
                }
            }
        }
    }
}
