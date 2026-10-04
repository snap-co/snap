//! Native workers adapt the portable Transport loop to socket drivers.
//! Only these workers touch the execution gate. Drivers hold queue handles.
use super::Shared;
use snap_transport::runtime::{CarrierControl, Loop, Output};
use snap_transport::{
    Command, Error, Event, Invocation, Response,
    carrier::{Connection, Dispatch, Frame, Submission},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

/// Host IO before admission and periodically during an attachment. No invocation
/// is replayed. The host must durably fence uncertain refresh IO; owned callbacks
/// may finish after detach. This is host policy, not TCP driver behavior.
pub type Prepare =
    Arc<dyn Fn(Command) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send>> + Send + Sync>;

pub struct Dispatcher<L: Loop> {
    shared: Arc<Shared<L>>,
    prepare: Option<Prepare>,
    one_shot: bool,
}
impl<L: Loop> Clone for Dispatcher<L> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            prepare: self.prepare.clone(),
            one_shot: self.one_shot,
        }
    }
}
impl<L: Loop> Dispatcher<L> {
    pub fn web(shared: Arc<Shared<L>>) -> Self {
        Self {
            shared,
            prepare: None,
            one_shot: false,
        }
    }
    pub fn tcp(shared: Arc<Shared<L>>, prepare: Option<Prepare>) -> Self {
        Self {
            shared,
            prepare,
            one_shot: true,
        }
    }
}

pub struct Endpoint<L: Loop> {
    state: Arc<State<L>>,
    sender: mpsc::Sender<(Command, Reservation<L>)>,
}
struct State<L: Loop> {
    shared: Arc<Shared<L>>,
    output: Output,
    control: CarrierControl,
    retired: AtomicBool,
    pending: Mutex<(usize, usize)>,
    max_pending_bytes: usize,
    one_shot: bool,
}
// Reservations follow commands through the queue and a blocked worker. They are
// released only at the host admission boundary or when a command is discarded.
struct Reservation<L: Loop> {
    state: Arc<State<L>>,
    bytes: usize,
}
impl<L: Loop> Drop for Reservation<L> {
    fn drop(&mut self) {
        let mut pending = self.state.pending.lock().unwrap();
        pending.0 -= 1;
        pending.1 -= self.bytes;
    }
}
impl<L: Loop> State<L> {
    fn disconnect(&self) {
        self.control.detach(self.shared.now());
    }
    fn retire(&self, final_frame: Option<Frame>) {
        self.output.seal(final_frame);
        self.retired.store(true, Ordering::Release);
    }
    fn failure(&self, error: Error, handshake: bool, terminal: bool) {
        let frame = Frame {
            response: Response::Failed(error),
            handshake,
            attachment: None,
            terminal,
        };
        if terminal {
            self.retire(Some(frame));
        } else {
            self.output.push_frame(frame);
        }
    }
}
impl<L: Loop> Drop for Endpoint<L> {
    fn drop(&mut self) {
        self.state.disconnect();
    }
}
impl<L: Loop + Send + 'static> Connection for Endpoint<L> {
    fn submit(&self, command: Command, bytes: usize) -> Result<Submission, Error> {
        if matches!(command, Command::Close | Command::Disconnect) {
            if matches!(command, Command::Close) {
                self.state.control.close(self.state.shared.now());
            } else {
                self.state.disconnect();
            }
            return Ok(Submission::CloseSocket);
        }
        if self.state.retired.load(Ordering::Acquire) {
            return Err(Error::StaleConnection);
        }
        {
            let mut pending = self.state.pending.lock().unwrap();
            let total = pending
                .1
                .checked_add(bytes)
                .filter(|total| *total <= self.state.max_pending_bytes)
                .ok_or(Error::Capacity)?;
            if pending.0 >= 1024 {
                return Err(Error::Capacity);
            }
            pending.0 += 1;
            pending.1 = total;
        }
        let reservation = Reservation {
            state: self.state.clone(),
            bytes,
        };
        if let Err(error) = self.sender.try_send((command, reservation)) {
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => Error::Capacity,
                _ => Error::StaleConnection,
            });
        }
        Ok(Submission::Queued)
    }
    fn receive(&self) -> Option<Frame> {
        self.state.output.pop_frame().map(|mut frame| {
            frame.terminal |= self.state.one_shot && matches!(frame.response, Response::Failed(_));
            frame
        })
    }
    fn retired(&self) -> bool {
        self.state.retired.load(Ordering::Acquire)
    }
    fn disconnect(&self) {
        self.state.disconnect();
    }
}

impl<L: Loop + Send + 'static> Dispatch for Dispatcher<L> {
    type Connection = Endpoint<L>;
    async fn open(
        &self,
        credential: Option<String>,
        max_pending_bytes: usize,
    ) -> Result<Endpoint<L>, Error> {
        let shared = self.shared.clone();
        let one_shot = self.one_shot;
        let (channel, receiver, peer) = tokio::task::spawn_blocking(move || {
            let mut host = shared.host.lock().unwrap();
            if let Some(credential) = credential {
                host.authorize_upgrade(&credential)?;
            }
            let peer = host.open()?;
            let output = host.output(peer)?;
            let control = host.carrier_control(peer)?;
            let (sender, receiver) = mpsc::channel(1024);
            let state = Arc::new(State {
                shared: shared.clone(),
                output,
                control,
                retired: AtomicBool::new(false),
                pending: Mutex::new((0, 0)),
                max_pending_bytes,
                one_shot,
            });
            Ok::<_, Error>((Endpoint { state, sender }, receiver, peer))
        })
        .await
        .map_err(|_| Error::Unavailable)??;
        let state = channel.state.clone();
        let prepare = self.prepare.clone();
        tokio::spawn(async move {
            run(state, receiver, peer, prepare).await;
        });
        Ok(channel)
    }
    async fn request(
        &self,
        invocation: Invocation,
        bearer: Option<String>,
    ) -> snap_transport::bearer::Reply {
        let shared = self.shared.clone();
        tokio::task::spawn_blocking(move || {
            shared
                .host
                .lock()
                .unwrap()
                .preconnection_reply(invocation, bearer)
        })
        .await
        .unwrap_or_else(|_| Err(Error::Unavailable).into())
    }
}

struct Maintenance {
    // Dropping the sender stops idle maintenance, without cancelling uncertain IO.
    _stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), Error>>,
}
async fn prepared(prepare: &Prepare, command: Command) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(60), prepare(command))
        .await
        .map_err(|_| Error::Unavailable)?
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

async fn run<L: Loop + Send + 'static>(
    state: Arc<State<L>>,
    mut receiver: mpsc::Receiver<(Command, Reservation<L>)>,
    peer: u64,
    prepare: Option<Prepare>,
) {
    let mut sweep = tokio::time::interval(Duration::from_millis(2));
    let mut first = true;
    let mut maintenance: Option<Maintenance> = None;
    loop {
        tokio::select! {
            command = receiver.recv() => {
                let Some((command, reservation)) = command else { break; };
                let handshake = matches!(command, Command::Connect { .. });
                if first && matches!(command, Command::Connect { .. } | Command::Request { .. })
                    && let Some(prepare) = &prepare {
                    if let Err(error) = prepared(prepare, command.clone()).await {
                        let response = match &command {
                            Command::Request { invocation, .. } => Response::Event(Event::Completed { id: invocation.id, outcome: Err(error) }),
                            _ => Response::Failed(error),
                        };
                        state.retire(Some(Frame { response, handshake, attachment: None, terminal: true }));
                        break;
                    }
                    if handshake { maintenance = Some(Maintenance::start(prepare.clone(), command.clone())); }
                }
                let processing = state.clone();
                let fresh = first && receiver.is_empty();
                first = false;
                let terminal = tokio::task::spawn_blocking(move || {
                    let mut host = processing.shared.host.lock().unwrap();
                    drop(reservation);
                    // Socket teardown is consumed before protected admission,
                    // even if the command waited while a controller held the gate.
                    host.tick(processing.shared.now());
                    if host.retired(peer) { return true; }
                    if let Command::Request { invocation, bearer } = &command
                        && processing.one_shot && host.is_preconnection_request(&invocation.operation) {
                        if fresh {
                            let reply = host.preconnection_reply(invocation.clone(), bearer.clone());
                            let id = invocation.id;
                            let snap_transport::bearer::Reply { accepted, bearer: change, outcome } = reply;
                            // Transport carries one event per frame, so a
                            // preconnection reply that issues a credential is
                            // three frames. Publish the non-terminal
                            // observations first, then seal with the completion,
                            // which is what retirement means for this socket.
                            if accepted {
                                processing.output.push_frame(Frame {
                                    response: Response::Event(Event::Accepted { id }),
                                    handshake: false,
                                    attachment: None,
                                    terminal: false,
                                });
                            }
                            if let Some(change) = change {
                                processing.output.push_frame(Frame {
                                    response: Response::Event(Event::Bearer { id, change }),
                                    handshake: false,
                                    attachment: None,
                                    terminal: false,
                                });
                            }
                            processing.retire(Some(Frame {
                                response: Response::Event(Event::Completed { id, outcome }),
                                handshake: false,
                                attachment: None,
                                terminal: true,
                            }));
                        }
                        return true;
                    }
                    if let Err(error) = host.submit(peer, command, processing.shared.now()) {
                        processing.failure(error, handshake, processing.one_shot);
                        return processing.one_shot;
                    }
                    false
                }).await.unwrap_or_else(|_| { state.failure(Error::Unavailable, handshake, true); true });
                if terminal { state.retire(None); break; }
            }
            _ = sweep.tick() => {
                if let Some(maintenance) = &mut maintenance && maintenance.task.is_finished() {
                    let error = (&mut maintenance.task).await.ok().and_then(Result::err).unwrap_or(Error::InvalidBearer);
                    state.failure(error, false, true);
                    break;
                }
                if state.shared.host.try_lock().is_ok_and(|host| host.retired(peer)) {
                    state.retire(None);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

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
