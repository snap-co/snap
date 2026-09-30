//! Binary TCP feeds the same Host and dispatcher as WebSocket. Output and detach
//! use independent handles so controller IO never blocks ACK/progress delivery.
use crate::web::Shared;
use snap_store::Backend;
use snap_transport::{Command, Event, Response};
use snap_transport_native as io;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::net::TcpListener;

/// Host IO before initial admission and periodically during an attachment.
/// This may refresh credentials, but does not replace Host's transaction guards.
/// No application invocation is replayed. The callback must durably fence any
/// uncertain refresh IO; callbacks in flight are allowed to settle after detach.
pub type Prepare = Arc<
    dyn Fn(Command) -> Pin<Box<dyn Future<Output = Result<(), snap_transport::Error>> + Send>>
        + Send
        + Sync,
>;

pub async fn serve<B: Backend + Send + 'static>(
    listener: TcpListener,
    shared: Arc<Shared<B>>,
    tls: io::tls::ServerTls,
) -> std::io::Result<()> {
    serve_inner(listener, shared, tls, None).await
}

pub async fn serve_prepared<B: Backend + Send + 'static>(
    listener: TcpListener,
    shared: Arc<Shared<B>>,
    tls: io::tls::ServerTls,
    prepare: Prepare,
) -> std::io::Result<()> {
    serve_inner(listener, shared, tls, Some(prepare)).await
}

async fn serve_inner<B: Backend + Send + 'static>(
    listener: TcpListener,
    shared: Arc<Shared<B>>,
    tls: io::tls::ServerTls,
    prepare: Option<Prepare>,
) -> std::io::Result<()> {
    let permits = Arc::new(tokio::sync::Semaphore::new(128));
    let mut peers = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                let shared = shared.clone();
                let tls = tls.clone();
                let prepare = prepare.clone();
                peers.spawn(async move {
                    let _permit = permit;
                    if let Ok(socket) = tls.accept(socket).await { let _ = connection(socket, shared, prepare).await; }
                });
            },
            Some(_) = peers.join_next(), if !peers.is_empty() => {},
        }
    }
}

struct Maintenance {
    // Dropping the sender stops idle maintenance, without cancelling uncertain IO.
    _stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), snap_transport::Error>>,
}
async fn prepared(prepare: &Prepare, command: Command) -> Result<(), snap_transport::Error> {
    tokio::time::timeout(Duration::from_secs(60), prepare(command))
        .await
        .map_err(|_| snap_transport::Error::Unavailable)?
}
impl Maintenance {
    fn start(prepare: Prepare, command: Command) -> Self {
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stopped => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(15)) => {},
                }
                prepared(&prepare, command.clone()).await?;
            }
        });
        Self { _stop: stop, task }
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
    socket: io::tls::ServerStream,
    shared: Arc<Shared<B>>,
    prepare: Option<Prepare>,
) -> std::io::Result<()> {
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
    let (mut reader, mut writer) = tokio::io::split(socket);
    let mut pending = std::collections::VecDeque::new();
    let mut pending_bytes = 0usize;
    let mut flush = tokio::time::interval(Duration::from_millis(2));
    let mut connected = false;
    let mut submitted = false;
    let mut attachment = None;
    let mut maintenance: Option<Maintenance> = None;
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
                    if !connected && !submitted && pending.is_empty()
                        && matches!(command, Command::Connect { .. } | Command::Request { .. })
                        && let Some(prepare) = &prepare
                    {
                        if let Err(error) = prepared(prepare, command.clone()).await {
                            let response = match &command {
                                Command::Request { invocation, .. } => Response::Events(vec![Event::Completed { id: invocation.id, outcome: Err(error) }]),
                                _ => Response::Failed(error),
                            };
                            io::write_response(&mut writer, &response, matches!(command, Command::Connect { .. }), None).await?;
                            return Ok(());
                        }
                        if matches!(command, Command::Connect { .. }) {
                            maintenance = Some(Maintenance::start(prepare.clone(), command.clone()));
                        }
                    }
                    if matches!(command, Command::Connect { .. }) { connected = true; }
                    pending_bytes += size;
                    pending.push_back((command,size));
                    break;
                },
                _ = flush.tick() => {
                    if let Some(maintenance) = &mut maintenance && maintenance.task.is_finished() {
                        let error = (&mut maintenance.task).await.map_err(std::io::Error::other)?
                            .err().unwrap_or(snap_transport::Error::InvalidBearer);
                        io::write_response(&mut writer, &Response::Failed(error), false, None).await?;
                        return Ok(());
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test(start_paused = true)]
    async fn detached_maintenance_finishes_owned_refresh_without_starting_another() {
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let settled = Arc::new(tokio::sync::Notify::new());
        let prepare: Prepare = Arc::new({
            let calls = calls.clone();
            let entered = entered.clone();
            let release = release.clone();
            let settled = settled.clone();
            move |_| {
                let calls = calls.clone();
                let entered = entered.clone();
                let release = release.clone();
                let settled = settled.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    release.notified().await;
                    settled.notify_one();
                    Ok(())
                })
            }
        });
        let maintenance = Maintenance::start(
            prepare,
            Command::Connect {
                bearer: "private".into(),
                client_id: "logical".into(),
            },
        );
        entered.notified().await;
        drop(maintenance);
        release.notify_one();
        settled.notified().await;
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
