//! Development host adapter. The ordinary WebSocket driver holds only queues;
//! stepping, debugger publication and physical-lifetime policy stay here.
use super::{Host, Shared};
use snap_transport::{
    Command, Error, Invocation, Outcome, Response,
    carrier::{Connection, Dispatch, Frame, Submission},
    execution::Program,
    server::Authority,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

pub(super) struct Dispatcher<P: Program, R: Authority>(pub Host<P, R>);
impl<P: Program, R: Authority> Clone for Dispatcher<P, R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
pub(super) struct Endpoint {
    sender: mpsc::Sender<(Command, usize)>,
    state: Arc<State>,
}
struct State {
    output: Mutex<VecDeque<Frame>>,
    teardown: AtomicU8,
    retired: AtomicBool,
    pending_bytes: AtomicUsize,
    max_pending_bytes: usize,
    wake: tokio::sync::Notify,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.disconnect();
    }
}
impl Connection for Endpoint {
    fn submit(&self, command: Command, bytes: usize) -> Result<Submission, Error> {
        if matches!(command, Command::Close | Command::Disconnect) {
            self.state.teardown.fetch_max(
                if matches!(command, Command::Close) {
                    3
                } else {
                    2
                },
                Ordering::AcqRel,
            );
            self.state.wake.notify_one();
            return Ok(Submission::Queued);
        }
        if self.retired() {
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
        self.state.output.lock().unwrap().pop_front()
    }
    fn retired(&self) -> bool {
        self.state.retired.load(Ordering::Acquire)
    }
    fn disconnect(&self) {
        self.state.teardown.fetch_max(1, Ordering::AcqRel);
        self.state.wake.notify_one();
    }
}
impl<P: Program + Send + 'static, R: Authority + Send + 'static> Dispatch for Dispatcher<P, R> {
    type Connection = Endpoint;
    async fn open(&self, _: Option<String>, max_pending_bytes: usize) -> Result<Endpoint, Error> {
        let shared = self.0.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let peer = shared
                .change(crate::development::Development::open)
                .map_err(|_| Error::Capacity)?;
            let (sender, receiver) = mpsc::channel(1024);
            let state = Arc::new(State {
                output: Mutex::new(VecDeque::new()),
                teardown: AtomicU8::new(0),
                retired: AtomicBool::new(false),
                pending_bytes: AtomicUsize::new(0),
                max_pending_bytes,
                wake: tokio::sync::Notify::new(),
            });
            let worker = state.clone();
            runtime.spawn(async move {
                run(shared, peer, worker, receiver).await;
            });
            Ok(Endpoint { sender, state })
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn request(&self, _: Invocation, _: Option<String>) -> Outcome {
        Err(Error::UnknownOperation)
    }
}
async fn run<P: Program + Send + 'static, R: Authority + Send + 'static>(
    shared: Arc<Shared<P, R>>,
    peer: u64,
    state: Arc<State>,
    mut receiver: mpsc::Receiver<(Command, usize)>,
) {
    let mut sweep = tokio::time::interval(Duration::from_millis(50));
    let mut updates = shared.updates.subscribe();
    loop {
        let command = tokio::select! {
            command = receiver.recv() => {
                match command {
                    Some((command, bytes)) => { state.pending_bytes.fetch_sub(bytes, Ordering::AcqRel); Some(command) }
                    None => { state.teardown.fetch_max(1, Ordering::AcqRel); None }
                }
            }
            _ = sweep.tick() => None,
            _ = state.wake.notified() => None,
            _ = updates.changed() => None,
        };
        let processing = state.clone();
        let host = shared.clone();
        let retired = tokio::task::spawn_blocking(move || {
            let now = host.clock.elapsed().as_millis() as u64;
            host.change(|host| {
                match processing.teardown.load(Ordering::Acquire) {
                    signal @ (2 | 3) => {
                        let response = host.teardown(peer, signal == 3, now);
                        processing.output.lock().unwrap().push_back(Frame {
                            response,
                            handshake: false,
                            attachment: None,
                            terminal: true,
                        });
                        return true;
                    }
                    1 => {
                        host.lost(peer, now);
                        return true;
                    }
                    _ => {}
                }
                if let Some(command) = command
                    && host.send(peer, command, now).is_err()
                {
                    host.lost(peer, now);
                    return true;
                }
                if let Ok(responses) = host.drain(peer) {
                    processing
                        .output
                        .lock()
                        .unwrap()
                        .extend(responses.into_iter().map(|response| Frame {
                            handshake: matches!(response, Response::Attached { .. }),
                            response,
                            attachment: None,
                            terminal: false,
                        }));
                }
                host.retired(peer)
            })
        })
        .await
        .unwrap_or(true);
        if retired {
            state.retired.store(true, Ordering::Release);
            break;
        }
    }
}
