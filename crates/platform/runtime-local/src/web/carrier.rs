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
        atomic::{AtomicBool, AtomicU8, Ordering},
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
    sender: mpsc::Sender<(Command, Reservation)>,
    state: Arc<State>,
}
struct State {
    output: Mutex<VecDeque<Frame>>,
    teardown: AtomicU8,
    retired: AtomicBool,
    pending: Mutex<(usize, usize)>,
    max_pending_bytes: usize,
    wake: tokio::sync::Notify,
}
// Include the item held by a worker waiting for the execution gate. Removing it
// from mpsc must not free its ingress count or byte budget before admission.
struct Reservation {
    state: Arc<State>,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut pending = self.state.pending.lock().unwrap();
        pending.0 -= 1;
        pending.1 -= self.bytes;
    }
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
                pending: Mutex::new((0, 0)),
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
    mut receiver: mpsc::Receiver<(Command, Reservation)>,
) {
    let mut sweep = tokio::time::interval(Duration::from_millis(50));
    let mut updates = shared.updates.subscribe();
    loop {
        let command = tokio::select! {
            command = receiver.recv() => {
                match command {
                    Some(command) => Some(command),
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
                let command = command.map(|(command, reservation)| {
                    drop(reservation);
                    command
                });
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
                let retired = host.retired(peer);
                if retired {
                    host.lost(peer, now);
                }
                retired
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
