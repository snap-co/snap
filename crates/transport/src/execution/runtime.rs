//! Portable session-to-executor adapter. Physical drivers submit commands, drain
//! observations and supply external inputs; this module owns no IO or clock.
use crate::execution;
use crate::execution::{Call, Executor, OperationMode, PreparedRequest, Program, Scope, Ticket};
use crate::{
    Command, Error, Event, Response,
    server::{Attachment, Authority, ConnectionId, Server, Verified},
};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
};

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
        bearer: Option<crate::bearer::Change>,
    },
    Need {
        ticket: Ticket,
        key: String,
    },
}
/// Session-aware execution over the shared scheduler. Connected work uses the
/// logical connection as its scope; accepted work pins that lifetime until its
/// completion. The caller supplies monotonic transport time and external inputs.
pub struct Runtime<P: Program, R: Authority> {
    transport: Server<R>,
    execution: Executor<P>,
    invocations: BTreeMap<Ticket, u64>,
    connections: BTreeMap<Ticket, ConnectionId>,
    accepted: BTreeSet<Ticket>,
}
impl<P: Program, R: Authority> Runtime<P, R> {
    pub fn new(transport: Server<R>, execution: Executor<P>) -> Self {
        Self {
            transport,
            execution,
            invocations: BTreeMap::new(),
            connections: BTreeMap::new(),
            accepted: BTreeSet::new(),
        }
    }
    pub fn with_requests(
        mut self,
        recognizes: fn(&str) -> bool,
        requests: impl FnMut(&crate::Invocation, Option<&str>) -> Option<PreparedRequest>
        + Send
        + 'static,
    ) -> Self {
        self.execution = self.execution.with_requests(recognizes, requests);
        self
    }
    pub fn attached(&self, peer: &Peer) -> bool {
        peer.attachment
            .as_ref()
            .is_some_and(|attachment| self.transport.attached(attachment))
    }
    pub fn authorize_upgrade(&self, bearer: &str) -> Result<(), Error> {
        self.transport.identify(bearer).map(|_| ())
    }
    /// Assemble private module operations from the registered contracts.
    /// Classification separates preconnection flows from connected work; it does
    /// not make those carriers interchangeable or expose credentials in traces.
    pub fn with_operations(
        mut self,
        classify: impl Fn(&str) -> Option<OperationMode> + Send + 'static,
        requests: impl FnMut(&crate::Invocation, Option<&str>) -> Option<PreparedRequest>
        + Send
        + 'static,
    ) -> Self {
        self.execution = self.execution.with_operations(classify, requests);
        self
    }
    /// Private capability requests must not enter debugger submission traces.
    pub fn private_request(&self, ticket: Ticket) -> bool {
        self.execution.private_request(ticket)
    }
    pub fn retired(&self, peer: &Peer) -> bool {
        peer.attachment.is_some() && !self.attached(peer)
    }
    /// Logical connections still consuming transport capacity, including detached
    /// lifetimes retained for reconnect or pinned by accepted work.
    pub fn residents(&self) -> usize {
        self.transport.resident_count()
    }
    /// Advance caller-supplied transport time and report logical retirement.
    /// Retirement discards queued work; already accepted work keeps its pin and drains.
    pub fn tick(&mut self, now: u64) -> bool {
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
                if let Some(mode) = self.execution.classify_private(&invocation.operation) {
                    if mode != OperationMode::Preconnection {
                        return Submission::Ready(Response::Failed(Error::Protocol));
                    }
                    return match self.execution.submit_request(invocation, bearer, None) {
                        Ok(ticket) => {
                            self.invocations.insert(ticket, id);
                            Submission::Pending(ticket)
                        }
                        Err(error) => Submission::Ready(Response::Event(Event::Completed {
                            id,
                            outcome: Err(transport_error(error)),
                        })),
                    };
                }
                return self.enqueue(id, self.transport.request(bearer.as_deref(), invocation));
            }
            Command::Invoke(invocation) => {
                // Request-only operations must never enter the execution queue or
                // its inspectable trace, even when sent over the wrong command kind.
                let private = self.execution.classify_private(&invocation.operation);
                if private == Some(OperationMode::Preconnection) {
                    return Submission::Ready(Response::Failed(Error::Protocol));
                }
                let id = invocation.id;
                let dispatch = match &peer.attachment {
                    Some(attachment) => self.transport.invoke(attachment, invocation),
                    None => Err(Error::IdentityRequired),
                };
                if private == Some(OperationMode::Connected) {
                    let result = dispatch.and_then(|dispatch| {
                        let attachment = peer.attachment.as_ref().ok_or(Error::IdentityRequired)?;
                        let bearer = self.transport.bearer(attachment)?.into();
                        let connection = dispatch.connection.ok_or(Error::IdentityRequired)?;
                        let ticket = self.execution.submit_request(
                            dispatch.invocation,
                            Some(bearer),
                            Some(Scope(connection.0)),
                        )?;
                        self.invocations.insert(ticket, id);
                        self.connections.insert(ticket, connection);
                        Ok(ticket)
                    });
                    return match result {
                        Ok(ticket) => Submission::Pending(ticket),
                        Err(error) => Submission::Ready(Response::Event(Event::Completed {
                            id,
                            outcome: Err(error),
                        })),
                    };
                }
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
    fn enqueue(&mut self, id: u64, dispatch: Result<Verified, Error>) -> Submission {
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
            Err(error) => Submission::Ready(Response::Event(Event::Completed {
                id,
                outcome: Err(error),
            })),
        }
    }
    pub fn step(&mut self) -> Option<Observation> {
        // Live authority controls later admission. Transport retention prevents
        // retirement from cancelling work whose acceptance already captured it.
        self.transport.tick(0);
        self.retire();
        Some(match self.execution.step()? {
            execution::Event::Reserved(_) => {
                unreachable!("capabilities are driven by portable execution")
            }
            execution::Event::Accepted(ticket) => {
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
                    private: self.execution.private_request(ticket),
                    bearer: None,
                }
            }
            execution::Event::Need { ticket, key } => Observation::Need { ticket, key },
            execution::Event::Completed { ticket, outcome } => {
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
                    private: self.execution.private_request(ticket),
                    bearer: self.execution.take_bearer(),
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
        result: execution::Outcome,
    ) -> Result<(), execution::Error> {
        self.execution.supply(ticket, key, result)
    }
    pub fn pending_call(&self, ticket: Ticket) -> Option<&Call> {
        self.execution.pending_call(ticket)
    }
    pub fn pause(&mut self) {
        self.execution.pause();
    }
    pub fn inspect(&self) -> execution::Inspection<'_> {
        self.execution.inspect()
    }
    pub fn snapshot(&self) -> Result<execution::Snapshot, execution::Error> {
        self.execution.snapshot()
    }
    pub fn restore(&mut self, snapshot: &execution::Snapshot) -> Result<(), execution::Error> {
        self.execution.restore(snapshot)
    }
    pub fn resume(&mut self) {
        self.execution.resume();
    }
    pub fn replace(&mut self, program: P) -> Result<(), execution::Error> {
        self.execution.replace(program)
    }
    pub fn lost(&mut self, peer: &mut Peer, now: u64) {
        if let Some(attachment) = peer.attachment.take() {
            let _ = self.transport.disconnect(&attachment, now);
        }
        self.retire();
    }
}
fn transport_error(error: execution::Error) -> Error {
    if error == Error::InvalidState {
        Error::InvalidOutput
    } else {
        error
    }
}
