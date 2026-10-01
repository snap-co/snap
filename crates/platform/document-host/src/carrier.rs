//! Document composition adapts the portable carrier contract to this host.
//! Only these workers touch the execution gate. Drivers hold queue handles.
use crate::{CarrierControl, Output, web::Shared};
use snap_store::Backend;
use snap_transport::{
    Command, Error, Event, Invocation, Outcome, Response,
    carrier::{Connection, Dispatch, Frame, Submission},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

/// Host IO before admission and periodically during an attachment. No invocation
/// is replayed. The host must durably fence uncertain refresh IO; owned callbacks
/// may finish after detach. This is composition policy, not TCP driver behavior.
pub type Prepare =
    Arc<dyn Fn(Command) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send>> + Send + Sync>;

pub struct Dispatcher<B: Backend> {
    shared: Arc<Shared<B>>,
    prepare: Option<Prepare>,
    one_shot: bool,
}
impl<B: Backend> Clone for Dispatcher<B> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            prepare: self.prepare.clone(),
            one_shot: self.one_shot,
        }
    }
}
impl<B: Backend> Dispatcher<B> {
    pub(crate) fn web(shared: Arc<Shared<B>>) -> Self {
        Self {
            shared,
            prepare: None,
            one_shot: false,
        }
    }
    pub(crate) fn tcp(shared: Arc<Shared<B>>, prepare: Option<Prepare>) -> Self {
        Self {
            shared,
            prepare,
            one_shot: true,
        }
    }
}

pub struct Endpoint<B: Backend> {
    state: Arc<State<B>>,
    sender: mpsc::Sender<(Command, usize)>,
}
struct State<B: Backend> {
    shared: Arc<Shared<B>>,
    output: Output,
    control: CarrierControl,
    retired: AtomicBool,
    pending_bytes: AtomicUsize,
    max_pending_bytes: usize,
    one_shot: bool,
}
impl<B: Backend> State<B> {
    fn disconnect(&self) {
        self.control.detach(self.shared.now());
    }
    fn failure(&self, error: Error, handshake: bool) {
        self.output.push_frame(Frame {
            response: Response::Failed(error),
            handshake,
            attachment: None,
            terminal: self.one_shot,
        });
    }
}
impl<B: Backend> Drop for Endpoint<B> {
    fn drop(&mut self) {
        self.state.disconnect();
    }
}
impl<B: Backend + Send + 'static> Connection for Endpoint<B> {
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
        self.state
            .pending_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                pending
                    .checked_add(bytes)
                    .filter(|sum| *sum <= self.state.max_pending_bytes)
            })
            .map_err(|_| Error::Capacity)?;
        if let Err(error) = self.sender.try_send((command, bytes)) {
            self.state.pending_bytes.fetch_sub(bytes, Ordering::AcqRel);
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

impl<B: Backend + Send + 'static> Dispatch for Dispatcher<B> {
    type Connection = Endpoint<B>;
    async fn open(
        &self,
        credential: Option<String>,
        max_pending_bytes: usize,
    ) -> Result<Endpoint<B>, Error> {
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
                pending_bytes: AtomicUsize::new(0),
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
    async fn request(&self, invocation: Invocation, bearer: Option<String>) -> Outcome {
        let shared = self.shared.clone();
        tokio::task::spawn_blocking(move || {
            shared
                .host
                .lock()
                .unwrap()
                .preconnection_request(invocation, bearer)
        })
        .await
        .unwrap_or(Err(Error::Unavailable))
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

async fn run<B: Backend + Send + 'static>(
    state: Arc<State<B>>,
    mut receiver: mpsc::Receiver<(Command, usize)>,
    peer: u64,
    prepare: Option<Prepare>,
) {
    let mut sweep = tokio::time::interval(Duration::from_millis(2));
    let mut first = true;
    let mut maintenance: Option<Maintenance> = None;
    loop {
        tokio::select! {
            command = receiver.recv() => {
                let Some((command, bytes)) = command else { break; };
                state.pending_bytes.fetch_sub(bytes, Ordering::AcqRel);
                let handshake = matches!(command, Command::Connect { .. });
                if first && matches!(command, Command::Connect { .. } | Command::Request { .. })
                    && let Some(prepare) = &prepare {
                    if let Err(error) = prepared(prepare, command.clone()).await {
                        let response = match &command {
                            Command::Request { invocation, .. } => Response::Events(vec![Event::Completed { id: invocation.id, outcome: Err(error) }]),
                            _ => Response::Failed(error),
                        };
                        state.output.push_frame(Frame { response, handshake, attachment: None, terminal: true });
                        state.retired.store(true, Ordering::Release);
                        break;
                    }
                    if handshake { maintenance = Some(Maintenance::start(prepare.clone(), command.clone())); }
                }
                let processing = state.clone();
                let fresh = first && receiver.is_empty();
                first = false;
                let terminal = tokio::task::spawn_blocking(move || {
                    let mut host = processing.shared.host.lock().unwrap();
                    // Socket teardown is consumed before protected admission,
                    // even if the command waited while a controller held the gate.
                    host.tick(processing.shared.now());
                    if host.retired(peer) { return true; }
                    if let Command::Request { invocation, bearer } = &command
                        && processing.one_shot && host.is_preconnection_request(&invocation.operation) {
                        if fresh {
                            let outcome = host.preconnection_request(invocation.clone(), bearer.clone());
                            processing.output.push_frame(Frame { response: Response::Events(vec![Event::Completed { id: invocation.id, outcome }]), handshake: false, attachment: None, terminal: true });
                        }
                        return true;
                    }
                    if let Err(error) = host.submit(peer, command, processing.shared.now()) {
                        processing.failure(error, handshake);
                        return processing.one_shot;
                    }
                    false
                }).await.unwrap_or_else(|_| { state.failure(Error::Unavailable, handshake); true });
                if terminal { state.retired.store(true, Ordering::Release); break; }
            }
            _ = sweep.tick() => {
                if let Some(maintenance) = &mut maintenance && maintenance.task.is_finished() {
                    let error = (&mut maintenance.task).await.ok().and_then(Result::err).unwrap_or(Error::InvalidBearer);
                    state.failure(error, false);
                    state.retired.store(true, Ordering::Release);
                    break;
                }
                if state.shared.host.try_lock().is_ok_and(|host| host.retired(peer)) {
                    state.retired.store(true, Ordering::Release);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handoff_and_teardown_do_not_wait_for_the_execution_gate() {
        let migrations = [
            snap_access::MIGRATION,
            snap_document::server::MIGRATION,
            snap_document::server::LIFECYCLE_MIGRATION,
        ]
        .map(|source| toml::from_str(source).unwrap());
        let store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
        let document =
            snap_document::server::Document::new(snap_document::Registry::new(vec![]).unwrap());
        let host = crate::Host::new(
            store,
            document,
            Arc::new(|_, _| Ok("actor".into())),
            Default::default(),
            "boot".into(),
        );
        let shared = Shared::new(host, "http://localhost".into());
        let dispatcher = Dispatcher::web(shared.clone());
        let channel = Arc::new(dispatcher.open(None, 4).await.unwrap());
        // Holding the real host gate must not stop a carrier from returning to
        // socket work. No mock supplies admission or teardown ordering here.
        let gate = shared.host.lock().unwrap();
        let (finished, waiting) = std::sync::mpsc::channel();
        let socket = channel.clone();
        let thread = std::thread::spawn(move || {
            let command = || Command::Connect {
                bearer: "token".into(),
                client_id: "client".into(),
            };
            let oversized = socket.submit(command(), 5);
            let queued = socket.submit(command(), 4);
            let output = socket.receive();
            let close = socket.submit(Command::Close, 0);
            socket.disconnect();
            finished.send((oversized, queued, output, close)).unwrap();
        });
        let result = waiting.recv_timeout(Duration::from_secs(2));
        drop(gate);
        thread.join().unwrap();
        let (oversized, queued, output, close) =
            result.expect("carrier waited for application execution");
        assert_eq!(oversized, Err(Error::Capacity));
        assert_eq!(queued, Ok(Submission::Queued));
        assert_eq!(close, Ok(Submission::CloseSocket));
        assert!(output.is_none());
        tokio::time::timeout(Duration::from_secs(2), async {
            while !channel.retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            channel.receive().is_none(),
            "closed queued command reached admission"
        );
    }

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
