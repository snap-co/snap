//! Application-owned local platform. Transport and execution are selected here;
//! neither portable capability depends on the other. Hosts own all IO and state.
#[cfg(feature = "web")]
pub mod development;
pub mod memory;
#[cfg(feature = "native")]
pub mod native;
#[cfg(feature = "web")]
pub mod web;

use snap_execution::{Call, Executor, Program, Scope, Ticket};
use snap_transport::{
    Command, Error, Event, Response,
    server::{Attachment, Authority, ConnectionId, Dispatch, Server},
};
use std::collections::BTreeMap;

/// Application-selected synchronous request dispatcher. Runs under host exclusion;
/// it owns its validation and durable commit. It must not log request secrets.
/// Preparation validates authority without writes once its FIFO slot owns the gate.
/// The returned closure runs once, after acceptance is queued, with that authority.
/// Private request payloads and results never enter retained execution traces.
pub type PreparedRequest = Result<Box<dyn FnOnce() -> snap_transport::Outcome + Send>, Error>;
type Prepare =
    Box<dyn FnMut(&snap_transport::Invocation, Option<&str>) -> Option<PreparedRequest> + Send>;
struct Requests {
    recognizes: fn(&str) -> bool,
    prepare: Prepare,
}

#[derive(Default)]
pub struct Peer {
    attachment: Option<Attachment>,
}
pub enum Submission {
    Ready(Response),
    Pending(Ticket),
}
pub enum Observation {
    Event {
        ticket: Ticket,
        event: Event,
        private: bool,
    },
    Need {
        ticket: Ticket,
        key: String,
    },
}
pub struct Platform<P: Program, R: Authority> {
    transport: Server<R>,
    execution: Executor<P>,
    invocations: BTreeMap<Ticket, u64>,
    connections: BTreeMap<Ticket, ConnectionId>,
    accepted: std::collections::BTreeSet<Ticket>,
    requests: Option<Requests>,
    queued_requests: BTreeMap<Ticket, (snap_transport::Invocation, Option<String>)>,
    active_request: Option<(Ticket, Box<dyn FnOnce() -> snap_transport::Outcome + Send>)>,
}
impl<P: Program, R: Authority> Platform<P, R> {
    pub fn new(transport: Server<R>, execution: Executor<P>) -> Self {
        Self {
            transport,
            execution,
            invocations: BTreeMap::new(),
            connections: BTreeMap::new(),
            accepted: std::collections::BTreeSet::new(),
            requests: None,
            queued_requests: BTreeMap::new(),
            active_request: None,
        }
    }
    pub fn with_requests(
        mut self,
        recognizes: fn(&str) -> bool,
        requests: impl FnMut(&snap_transport::Invocation, Option<&str>) -> Option<PreparedRequest>
        + Send
        + 'static,
    ) -> Self {
        self.requests = Some(Requests {
            recognizes,
            prepare: Box::new(requests),
        });
        self
    }
    pub fn attached(&self, peer: &Peer) -> bool {
        peer.attachment
            .as_ref()
            .is_some_and(|attachment| self.transport.attached(attachment))
    }
    /// Private capability requests must not enter debugger submission traces.
    pub fn private_request(&self, ticket: Ticket) -> bool {
        self.queued_requests.contains_key(&ticket)
            || self
                .active_request
                .as_ref()
                .is_some_and(|(active, _)| *active == ticket)
    }
    pub fn retired(&self, peer: &Peer) -> bool {
        peer.attachment.is_some() && !self.attached(peer)
    }
    pub fn tick(&mut self, now: u64) {
        self.tick_retired(now);
    }
    pub(crate) fn tick_retired(&mut self, now: u64) -> bool {
        self.transport.tick(now);
        self.retire()
    }
    fn retire(&mut self) -> bool {
        let retired = self.transport.take_retired();
        let changed = !retired.is_empty();
        for connection in retired {
            self.execution.discard(Scope(connection.0));
        }
        changed
    }
    /// Only enqueues application work. Hosts must drive `step`, deliver its
    /// observations in order, and resolve Need outside application entry points.
    pub fn submit(&mut self, peer: &mut Peer, command: Command, now: u64) -> Submission {
        self.tick(now);
        let response = match command {
            Command::Connect { bearer, client_id } => {
                if peer.attachment.is_some() {
                    return Submission::Ready(Response::Failed(Error::Occupied));
                }
                match self.transport.connect(&bearer, &client_id, now) {
                    Ok((attachment, resumed)) => {
                        if !resumed {
                            self.execution
                                .open(Scope(attachment.connection().0))
                                .expect("fresh connection ID");
                        }
                        peer.attachment = Some(attachment);
                        Response::Attached { resumed }
                    }
                    Err(error) => Response::Failed(error),
                }
            }
            Command::Request { bearer, invocation } => {
                let id = invocation.id;
                if self
                    .requests
                    .as_ref()
                    .is_some_and(|handler| (handler.recognizes)(&invocation.operation))
                {
                    return match self.execution.reserve() {
                        Ok(ticket) => {
                            self.invocations.insert(ticket, id);
                            self.queued_requests.insert(ticket, (invocation, bearer));
                            Submission::Pending(ticket)
                        }
                        Err(error) => Submission::Ready(Response::Events(vec![Event::Completed {
                            id,
                            outcome: Err(transport_error(error)),
                        }])),
                    };
                }
                return self.enqueue(id, self.transport.request(bearer.as_deref(), invocation));
            }
            Command::Invoke(invocation) => {
                // Request-only operations must never enter the execution queue or
                // its inspectable trace, even when sent over the wrong command kind.
                if self
                    .requests
                    .as_ref()
                    .is_some_and(|handler| (handler.recognizes)(&invocation.operation))
                {
                    return Submission::Ready(Response::Failed(Error::Protocol));
                }
                let id = invocation.id;
                let dispatch = match &peer.attachment {
                    Some(attachment) => self.transport.invoke(attachment, invocation),
                    None => Err(Error::IdentityRequired),
                };
                return self.enqueue(id, dispatch);
            }
            Command::Disconnect | Command::Close => {
                let Some(attachment) = peer.attachment.take() else {
                    return Submission::Ready(Response::Failed(Error::StaleConnection));
                };
                let result = if matches!(command, Command::Close) {
                    self.transport.close(&attachment)
                } else {
                    self.transport.disconnect(&attachment, now)
                };
                self.retire();
                match result {
                    Ok(()) => Response::Detached,
                    Err(error) => Response::Failed(error),
                }
            }
        };
        Submission::Ready(response)
    }
    fn enqueue(&mut self, id: u64, dispatch: Result<Dispatch, Error>) -> Submission {
        let result = dispatch.and_then(|dispatch| {
            let connection = dispatch.connection;
            self.execution
                .submit(
                    dispatch.connection.map(|id| Scope(id.0)),
                    Call {
                        operation: dispatch.invocation.operation,
                        input: dispatch.invocation.input,
                        identity: dispatch.identity,
                    },
                )
                .map_err(transport_error)
                .inspect(|ticket| {
                    if let Some(connection) = connection {
                        self.connections.insert(*ticket, connection);
                    }
                })
        });
        match result {
            Ok(ticket) => {
                self.invocations.insert(ticket, id);
                Submission::Pending(ticket)
            }
            Err(error) => Submission::Ready(Response::Events(vec![Event::Completed {
                id,
                outcome: Err(error),
            }])),
        }
    }
    pub fn step(&mut self) -> Option<Observation> {
        if let Some((ticket, run)) = self.active_request.take() {
            let outcome = run();
            self.execution
                .finish_reserved(ticket)
                .expect("owned request slot");
            let id = self
                .invocations
                .remove(&ticket)
                .expect("owned request invocation");
            self.transport.tick(0);
            self.retire();
            return Some(Observation::Event {
                ticket,
                event: Event::Completed { id, outcome },
                private: true,
            });
        }
        // Live authority controls later admission. Transport retention prevents
        // retirement from cancelling work whose acceptance already captured it.
        self.transport.tick(0);
        self.retire();
        Some(match self.execution.step()? {
            snap_execution::Event::Reserved(ticket) => {
                let (invocation, bearer) =
                    self.queued_requests.remove(&ticket).expect("owned request");
                let prepared = (self.requests.as_mut().expect("configured requests").prepare)(
                    &invocation,
                    bearer.as_deref(),
                )
                .unwrap_or(Err(Error::UnknownOperation));
                let event = match prepared {
                    Ok(run) => {
                        self.active_request = Some((ticket, run));
                        Event::Accepted { id: invocation.id }
                    }
                    Err(error) => {
                        self.execution
                            .finish_reserved(ticket)
                            .expect("owned request slot");
                        self.invocations.remove(&ticket);
                        Event::Completed {
                            id: invocation.id,
                            outcome: Err(error),
                        }
                    }
                };
                Observation::Event {
                    ticket,
                    event,
                    private: true,
                }
            }
            snap_execution::Event::Accepted(ticket) => {
                if let Some(connection) = self.connections.get(&ticket) {
                    self.transport
                        .retain(*connection)
                        .expect("admitted live connection");
                }
                self.accepted.insert(ticket);
                Observation::Event {
                    ticket,
                    event: Event::Accepted {
                        id: self.invocations[&ticket],
                    },
                    private: false,
                }
            }
            snap_execution::Event::Need { ticket, key } => Observation::Need { ticket, key },
            snap_execution::Event::Completed { ticket, outcome } => {
                let id = self.invocations.remove(&ticket).expect("owned invocation");
                let accepted = self.accepted.remove(&ticket);
                if let Some(connection) = self.connections.remove(&ticket)
                    && accepted
                {
                    self.transport
                        .release(connection)
                        .expect("accepted connection pin");
                    self.retire();
                }
                Observation::Event {
                    ticket,
                    private: false,
                    event: Event::Completed {
                        id,
                        outcome: outcome.map_err(transport_error),
                    },
                }
            }
        })
    }
    pub fn supply(
        &mut self,
        ticket: Ticket,
        key: &str,
        result: snap_execution::Outcome,
    ) -> Result<(), snap_execution::Error> {
        self.execution.supply(ticket, key, result)
    }
    pub fn pending_call(&self, ticket: Ticket) -> Option<&Call> {
        self.execution.pending_call(ticket)
    }
    pub fn pause(&mut self) {
        self.execution.pause();
    }
    pub fn inspect(&self) -> snap_execution::Inspection<'_> {
        self.execution.inspect()
    }
    pub fn snapshot(&self) -> Result<snap_execution::Snapshot, snap_execution::Error> {
        self.execution.snapshot()
    }
    pub fn restore(
        &mut self,
        snapshot: &snap_execution::Snapshot,
    ) -> Result<(), snap_execution::Error> {
        self.execution.restore(snapshot)
    }
    pub fn resume(&mut self) {
        self.execution.resume();
    }
    pub fn replace(&mut self, program: P) -> Result<(), snap_execution::Error> {
        self.execution.replace(program)
    }
    pub fn lost(&mut self, peer: &mut Peer, now: u64) {
        if let Some(attachment) = peer.attachment.take() {
            let _ = self.transport.disconnect(&attachment, now);
        }
        self.retire();
    }
}
pub fn transport_error(error: snap_execution::Error) -> Error {
    match error {
        snap_execution::Error::UnknownOperation => Error::UnknownOperation,
        snap_execution::Error::IdentityRequired => Error::IdentityRequired,
        snap_execution::Error::InvalidInput => Error::InvalidInput,
        snap_execution::Error::InvalidOutput | snap_execution::Error::InvalidState => {
            Error::InvalidOutput
        }
        snap_execution::Error::Unavailable => Error::Unavailable,
        snap_execution::Error::Protocol => Error::Protocol,
        snap_execution::Error::Capacity => Error::Capacity,
        snap_execution::Error::Application(value) => Error::Application(value),
    }
}
