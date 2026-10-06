use super::{Action, Timeline};
use alloc::{
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::Arc,
    task::Wake,
};
use core::{
    cell::RefCell,
    future::{Future, poll_fn},
    pin::pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};
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
    Command { peer: u64, command: Command },
    Admit { peer: u64 },
    Execute,
    Deliver { peer: u64, response: Response },
    End { peer: u64 },
    Disconnect { peer: u64, close: bool },
    Maintenance,
}
struct Peer {
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
        self.network.borrow_mut().queue(
            self.timeline.now(),
            Event::Disconnect {
                peer: self.peer,
                close: false,
            },
        );
    }
}
impl snap_transport::Channel for Channel {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        let mut network = self.network.borrow_mut();
        let peer = network
            .peers
            .get_mut(&self.peer)
            .ok_or(Error::StaleConnection)?;
        if peer.lost || peer.retiring {
            return Err(Error::Unavailable);
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
        }
    }
    pub fn open(&mut self) -> Result<Channel, Error> {
        let peer = self.host.open()?;
        let output = self.host.output(peer)?;
        let control = self.host.carrier_control(peer)?;
        self.network.borrow_mut().peers.insert(
            peer,
            Peer {
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
            Event::Command { peer, command } => {
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
        let wake = Arc::new(RunWake(AtomicBool::new(true)));
        let waker = Waker::from(wake.clone());
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            self.check_time()?;
            if wake.0.swap(false, Ordering::SeqCst) {
                if self.polls >= self.timeline.schedule().max_polls {
                    return Err(Failure::PollLimit);
                }
                self.polls += 1;
                if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
                    self.finish()?;
                    return Ok(result);
                }
                if wake.0.load(Ordering::SeqCst) {
                    continue;
                }
            }
            if !self.step()? {
                return Err(Failure::Deadlock);
            }
        }
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
