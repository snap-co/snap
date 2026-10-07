use super::{Action, Timeline};
use crate::runner::{Drive, Host, Random};
use alloc::string::String;
use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::Arc,
    task::Wake,
};
use core::{
    cell::RefCell,
    future::{Future, poll_fn},
    pin::Pin,
    pin::pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};
use serde::Serialize;
use snap_transport::{
    Command, Error, Event as Observation, Response,
    carrier::Frame,
    runtime::{CarrierControl, Loop, Output},
};

/// Carrier setup policy, not a Transport-wide rule. Default matches native TCP's
/// terminal refusals and pending-command bound. WebSocket refusals can be reusable.
/// This models count reservations only, not wire-byte limits or socket buffering.
#[derive(Clone, Copy, Debug)]
pub struct CarrierPolicy {
    pub terminal_refusals: bool,
    pub max_pending_commands: usize,
}
impl Default for CarrierPolicy {
    fn default() -> Self {
        Self {
            terminal_refusals: true,
            max_pending_commands: 1024,
        }
    }
}
enum Event {
    Open {
        actor: usize,
        state: Rc<RefCell<OpenState>>,
    },
    Release {
        peer: u64,
    },
    Command {
        peer: u64,
        command: Command,
    },
    Admit {
        peer: u64,
    },
    Execute,
    Deliver {
        peer: u64,
        response: Response,
    },
    End {
        peer: u64,
    },
    Disconnect {
        peer: u64,
        close: bool,
    },
    Maintenance,
    Timer {
        id: u64,
        state: Rc<RefCell<Option<Waker>>>,
    },
    Task {
        id: u64,
    },
}

/// Loss boundaries use actual carrier/host observations, not invented results.
/// AfterAcceptance cuts when the carrier sees host Accepted, before the next step;
/// the client need not have observed that frame. If a synchronous step emits both
/// acceptance and completion, this cannot interleave inside that step.
/// BeforeCompletionDelivery cuts
/// after an admitted call publishes Completed, including rollback/rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum LossBoundary {
    BeforeAdmission,
    AfterAcceptance,
    BeforeCompletionDelivery,
}
#[derive(Clone, Debug)]
/// Permits one targeted outstanding invocation per peer. Parallel SDK calls on
/// one carrier require a multi-target fault policy.
pub struct NetworkFaults {
    pub seed: u64,
    /// Only this operation is targeted. Assembly can reserve fault-free reads
    /// and handshakes as its explicit recovery/fairness window.
    pub operation: String,
    /// Probability 1/one_in per sent invocation. Must be nonzero.
    pub one_in: u64,
    /// None selects a seeded boundary. Some pins the same mechanism for probes.
    pub boundary: Option<LossBoundary>,
}
struct FaultPolicy {
    config: NetworkFaults,
    random: Random,
}
#[derive(Clone, Copy)]
struct Loss {
    id: u64,
    boundary: LossBoundary,
    accepted: bool,
}

struct OpenState {
    result: Option<Result<Channel, Error>>,
    waiter: Option<Waker>,
}
/// Opening is scheduled on the same host executor as SDK traffic. No actor can
/// borrow the production host or allocate physical peers behind its scheduler.
#[derive(Clone)]
pub struct Connector {
    network: Rc<RefCell<Network>>,
    timeline: Timeline,
    actor: usize,
}
pub struct Opening {
    connector: Connector,
    key: (u64, u64),
    state: Rc<RefCell<OpenState>>,
}
impl Connector {
    pub fn open(&self) -> Opening {
        let mut network = self.network.borrow_mut();
        let key = (
            self.timeline.now(),
            network
                .sequence
                .checked_add(1)
                .expect("open sequence overflow"),
        );
        let state = Rc::new(RefCell::new(OpenState {
            result: None,
            waiter: None,
        }));
        network.queue(
            key.0,
            Event::Open {
                actor: self.actor,
                state: state.clone(),
            },
        );
        Opening {
            connector: self.clone(),
            key,
            state,
        }
    }
}
impl crate::runner::Reconnect<Channel> for Connector {
    fn open(&self) -> impl Future<Output = Result<Channel, Error>> {
        Connector::open(self)
    }
}
impl Future for Opening {
    type Output = Result<Channel, Error>;
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.borrow_mut();
        if let Some(result) = state.result.take() {
            Poll::Ready(result)
        } else {
            state.waiter = Some(context.waker().clone());
            Poll::Pending
        }
    }
}
impl Drop for Opening {
    fn drop(&mut self) {
        self.connector.network.borrow_mut().events.remove(&self.key);
        // Drop an allocated but unclaimed Channel outside the state borrow.
        let result = self.state.borrow_mut().result.take();
        drop(result);
    }
}

/// Cloneable access to the shared event scheduler, not a wall-clock timer.
#[derive(Clone)]
pub struct Clock {
    network: Rc<RefCell<Network>>,
    timeline: Timeline,
}
pub struct Sleep {
    clock: Clock,
    key: (u64, u64),
    waiter: Rc<RefCell<Option<Waker>>>,
}
impl Clock {
    pub fn now(&self) -> u64 {
        self.timeline.now()
    }
    pub fn sleep(&self, milliseconds: u64) -> Sleep {
        let at = self
            .now()
            .checked_add(milliseconds)
            .expect("timer deadline overflow");
        let waiter = Rc::new(RefCell::new(None));
        let mut network = self.network.borrow_mut();
        let id = network
            .sequence
            .checked_add(1)
            .expect("timer sequence overflow");
        network.queue(
            at,
            Event::Timer {
                id,
                state: waiter.clone(),
            },
        );
        self.timeline.record(Action::TimerScheduled {
            id,
            deadline_ms: at,
        });
        Sleep {
            clock: self.clone(),
            key: (at, id),
            waiter,
        }
    }
}
impl crate::runner::Timer for Clock {
    fn sleep(&self, milliseconds: u64) -> impl Future<Output = ()> {
        Clock::sleep(self, milliseconds)
    }
}
impl Future for Sleep {
    type Output = ();
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.clock.now() >= self.key.0 {
            Poll::Ready(())
        } else {
            *self.waiter.borrow_mut() = Some(context.waker().clone());
            Poll::Pending
        }
    }
}
impl Drop for Sleep {
    fn drop(&mut self) {
        if self
            .clock
            .network
            .borrow_mut()
            .events
            .remove(&self.key)
            .is_some()
        {
            self.clock
                .timeline
                .record(Action::TimerCanceled { id: self.key.1 });
        }
    }
}

type Callback<L> = Box<dyn FnMut(&mut L, u64)>;
struct Task<L> {
    label: &'static str,
    period_ms: Option<u64>,
    callback: Callback<L>,
}
struct Peer {
    actor: Option<usize>,
    loss: Option<Loss>,
    output: Output,
    control: CarrierControl,
    pending: VecDeque<Command>,
    admission_pending: bool,
    received: VecDeque<Response>,
    retiring: bool,
    lost: bool,
    command_at: u64,
    response_at: u64,
    waiter: Option<Waker>,
}
#[derive(Default)]
struct Network {
    faults: Option<FaultPolicy>,
    losses: [u64; 3],
    last_losses: BTreeMap<Option<usize>, super::Record>,
    events: BTreeMap<(u64, u64), Event>,
    sequence: u64,
    peers: BTreeMap<u64, Peer>,
    execution_pending: bool,
}
impl Network {
    fn queue(&mut self, at: u64, event: Event) {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("simulation event sequence exhausted");
        self.events.insert((at, self.sequence), event);
    }
}

/// Structured FIFO link. Sending queues an owned command without executing the
/// host. Carrier receipt and worker submission are separate scheduler events.
/// Receiving an empty live stream is Pending, never EOF. Physical loss never retries.
pub struct Channel {
    peer: u64,
    network: Rc<RefCell<Network>>,
    timeline: Timeline,
}
impl Channel {
    pub fn peer(&self) -> u64 {
        self.peer
    }
}
impl Drop for Channel {
    fn drop(&mut self) {
        self.network
            .borrow_mut()
            .queue(self.timeline.now(), Event::Release { peer: self.peer });
    }
}
impl snap_transport::Channel for Channel {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        let mut network = self.network.borrow_mut();
        let loss = if let Command::Invoke(invocation) = &command {
            network.faults.as_mut().and_then(|policy| {
                if invocation.operation != policy.config.operation
                    || policy.random.below(policy.config.one_in) != 0
                {
                    return None;
                }
                let boundary = policy.config.boundary.unwrap_or_else(|| {
                    [
                        LossBoundary::BeforeAdmission,
                        LossBoundary::AfterAcceptance,
                        LossBoundary::BeforeCompletionDelivery,
                    ][policy.random.below(3) as usize]
                });
                Some(Loss {
                    id: invocation.id,
                    boundary,
                    accepted: false,
                })
            })
        } else {
            None
        };
        let peer = network
            .peers
            .get_mut(&self.peer)
            .ok_or(Error::StaleConnection)?;
        if peer.lost || peer.retiring {
            return Err(Error::Unavailable);
        }
        if let Some(loss) = loss {
            assert!(
                peer.loss.is_none(),
                "fault policy supports one targeted call per physical peer"
            );
            peer.loss = Some(loss);
            self.timeline.record(Action::LossArmed {
                peer: self.peer,
                id: loss.id,
                boundary: loss.boundary,
            });
        }
        let at = self
            .timeline
            .now()
            .saturating_add(self.timeline.delay(self.timeline.schedule().command_ms));
        peer.command_at = peer.command_at.max(at);
        let at = peer.command_at;
        let (kind, id) = command_tag(&command);
        self.timeline.record(Action::CommandQueued {
            peer: self.peer,
            kind,
            id,
        });
        network.queue(
            at,
            Event::Command {
                peer: self.peer,
                command,
            },
        );
        Ok(())
    }
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        poll_fn(|context| {
            let mut network = self.network.borrow_mut();
            let Some(peer) = network.peers.get_mut(&self.peer) else {
                return Poll::Ready(Err(Error::StaleConnection));
            };
            // Orderly retirement drains already-delivered final output before EOF.
            // Explicit loss instead clears it in lose(), selecting an abort policy.
            if let Some(response) = peer.received.pop_front() {
                return Poll::Ready(Ok(Some(response)));
            }
            if peer.lost {
                return Poll::Ready(Err(Error::Unavailable));
            }
            peer.waiter = Some(context.waker().clone());
            Poll::Pending
        })
        .await
    }
}

#[derive(Debug, PartialEq)]
pub enum Failure {
    Deadlock,
    EventLimit,
    PollLimit,
    TimeLimit,
}
struct RunWake(AtomicBool);
impl Wake for RunWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// One production host, multiple physical peers, no threads or physical IO.
/// Events order by virtual deadline then insertion sequence. Synchronous host
/// steps block all events; no delivery can interleave inside a host callback.
pub struct Simulation<L: Loop> {
    host: L,
    network: Rc<RefCell<Network>>,
    timeline: Timeline,
    policy: CarrierPolicy,
    paused: bool,
    work_pending: bool,
    events: usize,
    polls: usize,
    tasks: BTreeMap<u64, Task<L>>,
    next_task: u64,
}
impl<L: Loop> Simulation<L> {
    pub fn new(host: L, timeline: Timeline) -> Self {
        Self::with_policy(host, timeline, Default::default())
    }
    pub fn with_policy(host: L, timeline: Timeline, policy: CarrierPolicy) -> Self {
        Self {
            host,
            network: Default::default(),
            timeline,
            policy,
            paused: false,
            work_pending: false,
            events: 0,
            polls: 0,
            tasks: Default::default(),
            next_task: 0,
        }
    }
    pub fn clock(&self) -> Clock {
        Clock {
            network: self.network.clone(),
            timeline: self.timeline.clone(),
        }
    }
    pub fn connector(&self, actor: usize) -> Connector {
        Connector {
            network: self.network.clone(),
            timeline: self.timeline.clone(),
            actor,
        }
    }
    pub fn network_faults(&mut self, config: NetworkFaults) {
        assert!(
            config.one_in > 0,
            "network fault probability needs a nonzero denominator"
        );
        self.network.borrow_mut().faults = Some(FaultPolicy {
            random: Random::stream(config.seed, "network-loss"),
            config,
        });
    }
    pub fn network_losses(&self) -> [u64; 3] {
        self.network.borrow().losses
    }
    /// One last loss per actor label survives bounded trace eviction and peer
    /// teardown. Anonymous peers share one diagnostic slot, not an unbounded log.
    pub fn last_losses(&self) -> alloc::vec::Vec<super::Record> {
        self.network
            .borrow()
            .last_losses
            .values()
            .cloned()
            .collect()
    }
    /// Schedule actual host work between synchronous steps. Callbacks may submit
    /// real operations; they must not fabricate observations. Repeated tasks use
    /// fixed delay from callback completion, avoiding unbounded catch-up bursts.
    /// A periodic task keeps finish() non-idle until canceled; use Host::drive for
    /// bounded campaigns, then cancel it before draining teardown.
    pub fn schedule_task(
        &mut self,
        at_ms: u64,
        period_ms: Option<u64>,
        label: &'static str,
        callback: impl FnMut(&mut L, u64) + 'static,
    ) -> u64 {
        assert!(period_ms != Some(0), "periodic task must advance time");
        self.next_task = self
            .next_task
            .checked_add(1)
            .expect("task sequence overflow");
        let id = self.next_task;
        self.tasks.insert(
            id,
            Task {
                label,
                period_ms,
                callback: Box::new(callback),
            },
        );
        self.network
            .borrow_mut()
            .queue(at_ms.max(self.timeline.now()), Event::Task { id });
        id
    }
    pub fn cancel_task(&mut self, id: u64) {
        self.tasks.remove(&id);
        self.network
            .borrow_mut()
            .events
            .retain(|_, event| !matches!(event, Event::Task { id: queued } if *queued == id));
    }
    pub fn open(&mut self) -> Result<Channel, Error> {
        self.open_peer(None)
    }
    pub fn open_for(&mut self, actor: usize) -> Result<Channel, Error> {
        self.open_peer(Some(actor))
    }
    fn open_peer(&mut self, actor: Option<usize>) -> Result<Channel, Error> {
        let peer = self.host.open()?;
        let output = self.host.output(peer)?;
        let control = self.host.carrier_control(peer)?;
        self.network.borrow_mut().peers.insert(
            peer,
            Peer {
                actor,
                loss: None,
                output,
                control,
                pending: Default::default(),
                admission_pending: false,
                received: Default::default(),
                retiring: false,
                lost: false,
                command_at: 0,
                response_at: 0,
                waiter: None,
            },
        );
        self.timeline.record(Action::PeerOpened { peer, actor });
        Ok(Channel {
            peer,
            network: self.network.clone(),
            timeline: self.timeline.clone(),
        })
    }
    /// Hold the host execution gate between calls, without stopping carrier
    /// receipt, output delivery or teardown. This cannot suspend a running callback.
    pub fn pause_host(&mut self, paused: bool) {
        self.paused = paused;
        if !paused {
            self.host.tick(self.timeline.now());
            self.publish();
            let peers: alloc::vec::Vec<_> = self.network.borrow().peers.keys().copied().collect();
            for peer in peers {
                self.admit_later(peer);
            }
            self.execute_later();
        }
    }
    pub fn disconnect(&mut self, peer: u64) {
        self.network.borrow_mut().queue(
            self.timeline.now(),
            Event::Disconnect { peer, close: false },
        );
    }
    /// Explicit idle-time input. Drive all events due through the new deadline,
    /// then tick host maintenance. A synchronous host call may overrun that time.
    pub fn advance_by(&mut self, milliseconds: u64) -> Result<(), Failure> {
        let until = self.timeline.now().saturating_add(milliseconds);
        self.network.borrow_mut().queue(until, Event::Maintenance);
        while self
            .network
            .borrow()
            .events
            .first_key_value()
            .is_some_and(|((at, _), _)| *at <= until)
        {
            self.step()?;
        }
        Ok(())
    }
    fn admit_later(&mut self, peer: u64) {
        if self.paused {
            return;
        }
        let mut network = self.network.borrow_mut();
        let state = network.peers.get_mut(&peer).expect("scheduled peer");
        if !state.lost && !state.retiring && !state.pending.is_empty() && !state.admission_pending {
            state.admission_pending = true;
            let at = self
                .timeline
                .now()
                .saturating_add(self.timeline.delay(self.timeline.schedule().admission_ms));
            network.queue(at, Event::Admit { peer });
        }
    }
    fn execute_later(&mut self) {
        if self.paused || !self.work_pending {
            return;
        }
        let mut network = self.network.borrow_mut();
        if !network.execution_pending {
            network.execution_pending = true;
            let at = self
                .timeline
                .now()
                .saturating_add(self.timeline.delay(self.timeline.schedule().execution_ms));
            network.queue(at, Event::Execute);
        }
    }
    fn publish(&mut self) {
        let mut network = self.network.borrow_mut();
        let mut events = alloc::vec::Vec::new();
        let mut losses = alloc::vec::Vec::new();
        for (&id, peer) in &mut network.peers {
            if peer.lost || peer.retiring {
                continue;
            }
            let mut retiring = self.host.retired(id);
            if retiring {
                peer.output.seal(None);
            }
            while let Some(mut frame) = peer.output.pop_frame() {
                frame.terminal |=
                    self.policy.terminal_refusals && matches!(frame.response, Response::Failed(_));
                let (kind, invocation) = response_tag(&frame.response);
                self.timeline.record(Action::ResponsePublished {
                    peer: id,
                    kind,
                    id: invocation,
                });
                if let Some(loss) = &mut peer.loss
                    && invocation == Some(loss.id)
                {
                    loss.accepted |= kind == "accepted";
                    let cut = (kind == "accepted"
                        && loss.boundary == LossBoundary::AfterAcceptance)
                        || (kind == "completed"
                            && loss.accepted
                            && loss.boundary == LossBoundary::BeforeCompletionDelivery);
                    if cut {
                        losses.push((id, *loss));
                        peer.loss = None;
                        break;
                    }
                    if kind == "completed" {
                        peer.loss = None;
                    }
                }
                let at = self
                    .timeline
                    .now()
                    .saturating_add(self.timeline.delay(self.timeline.schedule().response_ms));
                peer.response_at = peer.response_at.max(at);
                events.push((
                    peer.response_at,
                    Event::Deliver {
                        peer: id,
                        response: frame.response,
                    },
                ));
                if frame.terminal {
                    retiring = true;
                    peer.output.seal(None);
                    // A terminal frame is the last observation, even if a host
                    // queued another frame before the carrier saw the terminal.
                    while peer.output.pop_frame().is_some() {}
                    break;
                }
            }
            if retiring {
                peer.retiring = true;
                peer.pending.clear();
                events.push((
                    peer.response_at.max(self.timeline.now()),
                    Event::End { peer: id },
                ));
            }
        }
        for (at, event) in events {
            network.queue(at, event);
        }
        drop(network);
        for (peer, loss) in losses {
            self.cut(peer, loss);
        }
    }
    fn cut(&mut self, peer: u64, loss: Loss) {
        let index = match loss.boundary {
            LossBoundary::BeforeAdmission => 0,
            LossBoundary::AfterAcceptance => 1,
            LossBoundary::BeforeCompletionDelivery => 2,
        };
        let actor = self.network.borrow().peers[&peer].actor;
        let action = Action::NetworkLoss {
            peer,
            actor,
            id: loss.id,
            boundary: loss.boundary,
        };
        {
            let mut network = self.network.borrow_mut();
            network.losses[index] += 1;
            network.last_losses.insert(
                actor,
                super::Record {
                    at_ms: self.timeline.now(),
                    action: action.clone(),
                },
            );
        }
        self.timeline.record(action);
        self.lose(peer, false, false);
    }
    /// Drive one boundary. A command's receipt does not perform admission or IO.
    pub fn step(&mut self) -> Result<bool, Failure> {
        self.check_time()?;
        if !self.network.borrow().events.is_empty()
            && self.events >= self.timeline.schedule().max_events
        {
            return Err(Failure::EventLimit);
        }
        let Some(((at, _), event)) = self.network.borrow_mut().events.pop_first() else {
            return Ok(false);
        };
        self.events += 1;
        self.timeline.advance_to(at);
        self.check_time()?;
        if !self.paused {
            self.host.tick(self.timeline.now());
            self.publish();
        }
        match event {
            Event::Open { actor, state } => {
                let result = self.open_peer(Some(actor));
                let waiter = {
                    let mut state = state.borrow_mut();
                    state.result = Some(result);
                    state.waiter.take()
                };
                if let Some(waiter) = waiter {
                    waiter.wake();
                }
            }
            Event::Release { peer } => {
                self.lose(peer, false, false);
                let mut network = self.network.borrow_mut();
                network.peers.remove(&peer);
                network.events.retain(|_, event| {
                    !matches!(event,
                    Event::Command { peer: id, .. } | Event::Admit { peer: id } |
                    Event::Deliver { peer: id, .. } | Event::End { peer: id } |
                    Event::Disconnect { peer: id, .. } | Event::Release { peer: id } if *id == peer)
                });
            }
            Event::Command { peer, command } => {
                let loss = {
                    let mut network = self.network.borrow_mut();
                    let state = network.peers.get_mut(&peer).unwrap();
                    if state.loss.is_some_and(|loss| {
                        loss.boundary == LossBoundary::BeforeAdmission
                            && command_tag(&command).1 == Some(loss.id)
                    }) {
                        state.loss.take()
                    } else {
                        None
                    }
                };
                if let Some(loss) = loss {
                    self.cut(peer, loss);
                }
                let active = {
                    let network = self.network.borrow();
                    let state = &network.peers[&peer];
                    !state.lost && !state.retiring
                };
                if active {
                    let (kind, id) = command_tag(&command);
                    self.timeline
                        .record(Action::CommandDelivered { peer, kind, id });
                    if matches!(command, Command::Close | Command::Disconnect) {
                        self.lose(peer, matches!(command, Command::Close), false);
                    } else if self.network.borrow().peers[&peer].pending.len()
                        >= self.policy.max_pending_commands
                    {
                        // Native TCP closes on a carrier handoff capacity failure.
                        // Never admit overflow or synthesize an operation result.
                        self.lose(peer, false, false);
                    } else {
                        self.network
                            .borrow_mut()
                            .peers
                            .get_mut(&peer)
                            .unwrap()
                            .pending
                            .push_back(command);
                        self.admit_later(peer);
                    }
                }
            }
            Event::Admit { peer } => {
                let command = {
                    let mut network = self.network.borrow_mut();
                    let state = network.peers.get_mut(&peer).expect("scheduled peer");
                    state.admission_pending = false;
                    if self.paused || state.lost || state.retiring {
                        None
                    } else {
                        state.pending.pop_front()
                    }
                };
                if let Some(command) = command {
                    let (kind, id) = command_tag(&command);
                    let handshake = matches!(command, Command::Connect { .. });
                    self.timeline
                        .record(Action::CommandSubmitted { peer, kind, id });
                    if let Err(error) = self.host.submit(peer, command, self.timeline.now()) {
                        let output = self.network.borrow().peers[&peer].output.clone();
                        let frame = Frame {
                            response: Response::Failed(error),
                            handshake,
                            attachment: None,
                            terminal: self.policy.terminal_refusals,
                        };
                        if frame.terminal {
                            output.seal(Some(frame));
                        } else {
                            output.push_frame(frame);
                        }
                    }
                    self.work_pending = true;
                    self.execute_later();
                    self.admit_later(peer);
                }
            }
            Event::Execute => {
                self.network.borrow_mut().execution_pending = false;
                if !self.paused {
                    let progressed = self.host.step();
                    self.timeline.record(Action::Executed { progressed });
                    self.work_pending = progressed;
                    self.execute_later();
                }
            }
            Event::Deliver { peer, response } => {
                let mut network = self.network.borrow_mut();
                let state = network.peers.get_mut(&peer).expect("scheduled peer");
                if !state.lost {
                    let (kind, id) = response_tag(&response);
                    self.timeline
                        .record(Action::ResponseDelivered { peer, kind, id });
                    state.received.push_back(response);
                    let waiter = state.waiter.take();
                    drop(network);
                    if let Some(waiter) = waiter {
                        waiter.wake();
                    }
                }
            }
            Event::End { peer } => self.lose(peer, false, true),
            Event::Disconnect { peer, close } => self.lose(peer, close, false),
            Event::Maintenance => {}
            Event::Timer { id, state } => {
                self.timeline.record(Action::TimerFired { id });
                let waiter = state.borrow_mut().take();
                if let Some(waiter) = waiter {
                    waiter.wake();
                }
            }
            Event::Task { id } => {
                if let Some(mut task) = self.tasks.remove(&id) {
                    if self.paused {
                        let at = self
                            .timeline
                            .now()
                            .checked_add(task.period_ms.unwrap_or(1))
                            .expect("task deadline overflow");
                        self.tasks.insert(id, task);
                        self.network.borrow_mut().queue(at, Event::Task { id });
                        return Ok(true);
                    }
                    self.timeline.record(Action::TaskFired {
                        id,
                        label: task.label,
                    });
                    (task.callback)(&mut self.host, self.timeline.now());
                    self.work_pending = true;
                    self.execute_later();
                    if let Some(period) = task.period_ms {
                        let at = self
                            .timeline
                            .now()
                            .checked_add(period)
                            .expect("task deadline overflow");
                        self.tasks.insert(id, task);
                        self.network.borrow_mut().queue(at, Event::Task { id });
                    }
                }
            }
        }
        if !self.paused {
            self.host.tick(self.timeline.now());
            self.publish();
        }
        self.check_time()?;
        Ok(true)
    }
    fn lose(&mut self, peer: u64, close: bool, drain: bool) {
        let mut network = self.network.borrow_mut();
        if let Some(state) = network.peers.get_mut(&peer)
            && !state.lost
        {
            state.lost = true;
            state.pending.clear();
            state.output.seal(None);
            if !drain {
                state.received.clear();
            }
            if close {
                state.control.close(self.timeline.now());
            } else {
                state.control.detach(self.timeline.now());
            }
            self.timeline.record(Action::Disconnected { peer, close });
            let waiter = state.waiter.take();
            drop(network);
            if let Some(waiter) = waiter {
                waiter.wake();
            }
        }
    }
    fn check_time(&self) -> Result<(), Failure> {
        if self.timeline.now() > self.timeline.schedule().max_time_ms {
            Err(Failure::TimeLimit)
        } else {
            Ok(())
        }
    }
    pub fn finish(&mut self) -> Result<(), Failure> {
        self.check_time()?;
        while self.step()? {}
        Ok(())
    }
    /// Run one client task with wake-driven polling. Self-wakes are bounded by a
    /// separate poll budget; events do not implicitly wake clients. A Pending task
    /// without wakes or scheduler events is a diagnosed deadlock. Completion drains
    /// queued events, but deliberately paused host work remains paused.
    pub fn run<F: Future>(&mut self, future: F) -> Result<F::Output, Failure> {
        match self.drive_until(future, None)? {
            Drive::Complete(result) => {
                self.finish()?;
                Ok(result)
            }
            Drive::Deadline => unreachable!("no deadline selected"),
        }
    }
    fn drive_until<F: Future>(
        &mut self,
        future: F,
        deadline_ms: Option<u64>,
    ) -> Result<Drive<F::Output>, Failure> {
        let wake = Arc::new(RunWake(AtomicBool::new(true)));
        let waker = Waker::from(wake.clone());
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            self.check_time()?;
            if deadline_ms.is_some_and(|deadline| self.timeline.now() >= deadline) {
                return Ok(Drive::Deadline);
            }
            if wake.0.swap(false, Ordering::SeqCst) {
                if self.polls >= self.timeline.schedule().max_polls {
                    return Err(Failure::PollLimit);
                }
                self.polls += 1;
                if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
                    return Ok(Drive::Complete(result));
                }
                if wake.0.load(Ordering::SeqCst) {
                    continue;
                }
            }
            if let Some(deadline) = deadline_ms {
                let next = self
                    .network
                    .borrow()
                    .events
                    .first_key_value()
                    .map(|((at, _), _)| *at);
                if next.is_some_and(|at| at >= deadline) {
                    self.timeline.advance_to(deadline);
                    self.check_time()?;
                    return Ok(Drive::Deadline);
                }
            }
            if !self.step()? {
                return Err(Failure::Deadlock);
            }
        }
    }
}
impl<L: Loop> Host for Simulation<L> {
    type Error = Failure;
    fn now(&self) -> u64 {
        self.timeline.now()
    }
    fn drive<F: Future>(
        &mut self,
        future: F,
        deadline_ms: Option<u64>,
    ) -> Result<Drive<F::Output>, Failure> {
        self.drive_until(future, deadline_ms)
    }
}
fn command_tag(command: &Command) -> (&'static str, Option<u64>) {
    match command {
        Command::Connect { .. } => ("connect", None),
        Command::Request { invocation, .. } => ("request", Some(invocation.id)),
        Command::Invoke(invocation) => ("invoke", Some(invocation.id)),
        Command::Disconnect => ("disconnect", None),
        Command::Close => ("close", None),
    }
}
fn response_tag(response: &Response) -> (&'static str, Option<u64>) {
    match response {
        Response::Attached { .. } => ("attached", None),
        Response::Detached => ("detached", None),
        Response::Failed(_) => ("refused", None),
        Response::Global { .. } => ("global", None),
        Response::Event(event) => match event {
            Observation::Accepted { id } => ("accepted", Some(*id)),
            Observation::Completed { id, .. } => ("completed", Some(*id)),
            Observation::Progress { id, .. } => ("progress", Some(*id)),
            Observation::Bearer { id, .. } => ("bearer", Some(*id)),
        },
    }
}
