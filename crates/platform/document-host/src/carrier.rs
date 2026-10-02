//! Document composition adapts the portable carrier contract to this host.
//! Only these workers touch the execution gate. Drivers hold queue handles.
use crate::{CarrierControl, Output, web::Shared};
use snap_store::Backend;
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
    sender: mpsc::Sender<(Command, Reservation<B>)>,
}
struct State<B: Backend> {
    shared: Arc<Shared<B>>,
    output: Output,
    control: CarrierControl,
    retired: AtomicBool,
    pending: Mutex<(usize, usize)>,
    max_pending_bytes: usize,
    one_shot: bool,
}
// Reservations follow commands through the queue and a blocked worker. They are
// released only at the host admission boundary or when a command is discarded.
struct Reservation<B: Backend> {
    state: Arc<State<B>>,
    bytes: usize,
}
impl<B: Backend> Drop for Reservation<B> {
    fn drop(&mut self) {
        let mut pending = self.state.pending.lock().unwrap();
        pending.0 -= 1;
        pending.1 -= self.bytes;
    }
}
impl<B: Backend> State<B> {
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

async fn run<B: Backend + Send + 'static>(
    state: Arc<State<B>>,
    mut receiver: mpsc::Receiver<(Command, Reservation<B>)>,
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
                            Command::Request { invocation, .. } => Response::Events(vec![Event::Completed { id: invocation.id, outcome: Err(error) }]),
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
                            let mut events = Vec::new();
                            if reply.accepted { events.push(Event::Accepted { id: invocation.id }); }
                            if let Some(change) = reply.bearer { events.push(Event::Bearer { id: invocation.id, change }); }
                            events.push(Event::Completed { id: invocation.id, outcome: reply.outcome });
                            processing.retire(Some(Frame { response: Response::Events(events), handshake: false, attachment: None, terminal: true }));
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

    fn fixture() -> Arc<Shared<snap_store_sqlite::Sqlite>> {
        Shared::new(
            host_fixture(snap_document::server::Document::new(
                snap_document::Registry::new(vec![]).unwrap(),
            )),
            "http://localhost".into(),
        )
    }

    fn host_fixture(
        document: snap_document::server::Document,
    ) -> crate::Host<snap_store_sqlite::Sqlite> {
        let migrations = [
            snap_access::MIGRATION,
            snap_document::server::MIGRATION,
            snap_document::server::LIFECYCLE_MIGRATION,
        ]
        .map(|source| toml::from_str(source).unwrap());
        let mut store = snap_store_sqlite::Sqlite::memory(&migrations).unwrap();
        for table in snap_access::TABLES
            .iter()
            .chain(snap_document::server::TABLES.iter())
        {
            store.load(table).unwrap();
        }
        crate::Host::new(
            store,
            document,
            Arc::new(|_, _| Ok("actor".into())),
            Default::default(),
            "boot".into(),
        )
    }

    #[tokio::test]
    async fn maintenance_retirement_seals_output_while_controller_finishes_and_completion_replays()
    {
        use snap_document::{
            Definition, Intent, Mutation, Registry, ServerMessage, Snapshot, server::Document,
        };
        use snap_transport::json;
        const ID: &str = "018f3c4b-6d2a-7000-8000-000000000001";
        let document = || {
            Document::new(
                Registry::new(vec![Definition {
                    kind: "counter".into(),
                    version: "1".into(),
                    validate: |value| value.is_i64(),
                    mutations: vec![Mutation {
                        name: "add".into(),
                        minimum: snap_access::Role::Editor,
                        guard: None,
                        apply: |value, args, _| {
                            Ok(json!(value.as_i64().unwrap() + args.as_i64().unwrap()))
                        },
                    }],
                }])
                .unwrap(),
            )
        };
        let mut host = host_fixture(document());
        let document = document();
        host.transact("seed", |tx| {
            document.create(
                tx,
                &Snapshot {
                    id: ID.into(),
                    kind: "counter".into(),
                    version: "1".into(),
                    revision: 1,
                    value: json!(0),
                },
                snap_access::Audience::Restricted,
                "actor",
            )
        })
        .unwrap();

        let (entered, waiting) = tokio::sync::oneshot::channel();
        let mut entered = Some(entered);
        let (finish, released) = std::sync::mpsc::channel::<()>();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let host = host.with_controller(
            "counter",
            Box::new(move |ctx, _| {
                count.fetch_add(1, Ordering::SeqCst);
                ctx.progress(json!("before retirement"))?;
                entered.take().unwrap().send(()).unwrap();
                let _ = released.recv_timeout(Duration::from_secs(10));
                ctx.progress(json!("after retirement"))?;
                Ok(())
            }),
        );
        let shared = Shared::new(host, "http://localhost".into());
        let preparation = Arc::new(AtomicUsize::new(0));
        let prepare: Prepare = Arc::new(move |_| {
            let first = preparation.fetch_add(1, Ordering::SeqCst) == 0;
            Box::pin(async move {
                if first {
                    Ok(())
                } else {
                    Err(Error::InvalidBearer)
                }
            })
        });
        let channel = Dispatcher::tcp(shared.clone(), Some(prepare))
            .open(None, 4096)
            .await
            .unwrap();
        let connect = || Command::Connect {
            bearer: "token".into(),
            client_id: "retained".into(),
        };
        channel.submit(connect(), 1).unwrap();
        assert_eq!(
            next_frame(&channel).await.response,
            Response::Attached { resumed: false }
        );
        let invocation = Invocation {
            id: 1,
            operation: "document.mutate".into(),
            input: serde_json::to_value(Intent {
                id: 1,
                document: ID.into(),
                version: "1".into(),
                mutation: "add".into(),
                args: json!(1),
            })
            .unwrap(),
        };
        channel
            .submit(Command::Invoke(invocation.clone()), 1)
            .unwrap();
        assert_eq!(
            next_frame(&channel).await.response,
            Response::Events(vec![Event::Accepted { id: 1 }])
        );
        let executing = shared.clone();
        let worker = std::thread::spawn(move || executing.host.lock().unwrap().step());
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        // Advance the real maintenance timer while the controller holds the gate.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(15)).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !channel.retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let controller_still_running = !worker.is_finished();
        let final_frames: Vec<_> = std::iter::from_fn(|| channel.receive()).collect();
        // Release before assertions so even a red regression cannot strand work.
        drop(finish);
        assert!(worker.join().unwrap());
        tokio::time::resume();
        let late: Vec<_> = std::iter::from_fn(|| channel.receive()).collect();
        assert!(
            controller_still_running,
            "retirement waited for controller execution"
        );
        assert!(
            late.is_empty(),
            "retired physical outbox published late controller output: {late:?}"
        );
        assert!(final_frames.iter().any(|frame| frame.response
            == Response::Events(vec![Event::Progress {
                id: 1,
                value: json!("before retirement")
            }])));
        let final_frame = final_frames.last().unwrap();
        assert_eq!(final_frame.response, Response::Failed(Error::InvalidBearer));
        assert!(final_frame.terminal);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            shared
                .host
                .lock()
                .unwrap()
                .transact("read commit", |tx| document.retained(tx, ID))
                .unwrap()
                .value,
            json!(1)
        );

        // Socket loss does not discard the accepted result. Reconnect alone must
        // not publish it, but an explicit retry reattaches completion interest.
        channel.disconnect();
        let reopened = Dispatcher::web(shared.clone())
            .open(None, 4096)
            .await
            .unwrap();
        reopened.submit(connect(), 1).unwrap();
        assert_eq!(
            next_frame(&reopened).await.response,
            Response::Attached { resumed: true }
        );
        assert!(reopened.receive().is_none());
        reopened.submit(Command::Invoke(invocation), 1).unwrap();
        assert_eq!(
            next_frame(&reopened).await.response,
            Response::Events(vec![Event::Accepted { id: 1 }])
        );
        let Response::Events(events) = next_frame(&reopened).await.response else {
            panic!("missing completion replay");
        };
        let [
            Event::Completed {
                id: 1,
                outcome: Ok(value),
            },
        ] = events.as_slice()
        else {
            panic!("unexpected replay: {events:?}");
        };
        let ServerMessage::Completed(completion) = serde_json::from_value(value.clone()).unwrap()
        else {
            panic!("not a Document completion");
        };
        assert_eq!(completion.result.unwrap().unwrap().value, json!(1));
        assert!(!shared.host.lock().unwrap().step());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    async fn next_frame(channel: &Endpoint<snap_store_sqlite::Sqlite>) -> Frame {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(frame) = channel.receive() {
                    return frame;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("host did not publish a frame")
    }

    #[tokio::test]
    async fn handoff_and_teardown_do_not_wait_for_the_execution_gate() {
        let shared = fixture();
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_held_commands_keep_byte_and_count_reservations() {
        for (budget, bytes, additional) in [(4, 4, 0), (usize::MAX, 1, 1023)] {
            let shared = fixture();
            let (preparing, entered) = std::sync::mpsc::channel();
            let prepare: Prepare = Arc::new(move |_| {
                let preparing = preparing.clone();
                Box::pin(async move {
                    preparing.send(()).unwrap();
                    Ok(())
                })
            });
            let channel = Dispatcher::tcp(shared.clone(), Some(prepare))
                .open(None, budget)
                .await
                .unwrap();
            let command = || Command::Connect {
                bearer: "token".into(),
                client_id: "queued".into(),
            };
            let gate = shared.host.lock().unwrap();
            assert_eq!(channel.submit(command(), bytes), Ok(Submission::Queued));
            // Preparation is a production callback before admission. Its signal
            // proves the real worker took the command, without an introspection hook.
            entered.recv_timeout(Duration::from_secs(2)).unwrap();
            for _ in 0..additional {
                assert_eq!(channel.submit(command(), bytes), Ok(Submission::Queued));
            }
            let overflow = channel.submit(command(), bytes);
            channel.submit(Command::Close, 0).unwrap();
            channel.disconnect();
            drop(gate);
            assert_eq!(overflow, Err(Error::Capacity));
        }
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
