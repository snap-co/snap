use crate::{Channel, Command, Error, Event, Invocation, Outcome, Response, Value};
use alloc::{boxed::Box, collections::BTreeMap, string::String};

/// Terminal operation calls shared by module SDKs. This is not connected IO.
/// Select a declared exception or the active connection before submission.
/// Failure never selects a second carrier or replays work.
pub trait Operations {
    fn call<O: crate::Operation>(
        &mut self,
        input: &O::Input,
    ) -> impl core::future::Future<Output = Result<O::Output, Error>>;
}

impl<C: Channel> Operations for Client<C> {
    async fn call<O: crate::Operation>(&mut self, input: &O::Input) -> Result<O::Output, Error> {
        let input = serde_json::to_value(input).map_err(|_| Error::InvalidInput)?;
        let value = if let Some((_, read_bearer)) = O::HTTP {
            let bearer = if read_bearer {
                self.bearer().map(String::from)
            } else {
                None
            };
            self.request(bearer.as_deref(), O::NAME, input).await?
        } else {
            self.invoke(O::NAME, input).await?
        };
        decode(value)
    }
}

/// Client-owned correlation IDs shared across module bindings. Keep the allocator
/// across physical reconnects; IDs grant no authority and imply no retry policy.
#[derive(Default)]
pub struct InvocationIds(u64);
impl InvocationIds {
    pub fn allocate(&mut self) -> Result<u64, Error> {
        self.0 = self.0.checked_add(1).ok_or(Error::Capacity)?;
        Ok(self.0)
    }
    pub fn invoke(&mut self, operation: &str, input: Value) -> Result<Command, Error> {
        Ok(Command::Invoke(Invocation {
            id: self.allocate()?,
            operation: operation.into(),
            input,
        }))
    }
}

/// Receives an uncorrelated server push for one topic kind.
///
/// Handlers are keyed by kind alone: a capability registers once and receives
/// every published payload of that kind. Transport never inspects the payload.
pub type Handler = Box<dyn FnMut(Value) + Send>;

/// What one [`Client::pump`] call did with a frame.
#[derive(Debug, PartialEq)]
pub enum Pump {
    /// An invocation finished. Its outcome is the operation's return value.
    Completed { id: u64, outcome: Outcome },
    /// Progress from an invocation still running.
    Progress { id: u64, value: Value },
    /// A correlated frame that neither completed nor progressed an invocation,
    /// such as acceptance.
    Accepted { id: u64 },
    /// A global push went to its handler.
    Global,
    /// The frame could not be placed: it named an invocation this client never
    /// sent, arrived after that invocation completed, or broke its ordering
    /// contract. Dropped rather than reported — a peer controls what it sends,
    /// so an unroutable frame is not the client's error.
    ///
    /// `id` is the invocation it named, when it named one of ours. That
    /// invocation is now unsatisfiable: its trace was abandoned, so nothing
    /// further can resolve it.
    Discarded(Option<u64>),
    /// The channel closed.
    Closed,
    /// The carrier refused a command before it began, so no invocation owns it.
    Refused(Error),
}

/// Correlation and lifecycle shared by every SDK/carrier.
///
/// Two server-to-client paths, told apart by whether the frame is correlated:
///
/// - **channel** — carries the invocation id the client minted when it sent the
///   command, and reaches only that originator. `Completed` terminates it.
/// - **global** — uncorrelated, routed by subscription, delivered to the handler
///   registered for its kind.
///
/// [`Self::pump`] consumes one frame and routes it. The caller drives the stream:
/// an operation outlives any single frame, so nothing here can await its own
/// reply without someone else pumping.
pub struct Client<C> {
    channel: C,
    sequence: InvocationIds,
    bearer: Option<crate::bearer::Token>,
    handlers: BTreeMap<String, Handler>,
    /// Outstanding invocations by client-minted id. Entries are removed on
    /// completion, so a late frame for a finished call finds nothing.
    calls: BTreeMap<u64, Trace>,
}

impl<C: Channel> Client<C> {
    pub fn new(channel: C) -> Self {
        Self {
            channel,
            sequence: InvocationIds::default(),
            bearer: None,
            handlers: BTreeMap::new(),
            calls: BTreeMap::new(),
        }
    }

    /// Register the destination for one kind of uncorrelated push, replacing any
    /// previous handler. Capabilities call this once during setup.
    pub fn on_global(
        &mut self,
        kind: impl Into<String>,
        handler: impl FnMut(Value) + Send + 'static,
    ) {
        self.handlers.insert(kind.into(), Box::new(handler));
    }

    pub fn bearer(&self) -> Option<&str> {
        self.bearer.as_ref().map(crate::bearer::Token::expose)
    }
    pub fn use_bearer(&mut self, bearer: &str) {
        self.bearer = Some(crate::bearer::Token::new(bearer.into()));
    }
    /// Adopt a replacement physical attachment under the same logical connection.
    ///
    /// Outstanding correlation traces are retained, but Transport does not recover
    /// their results or resend commands. Each trace restarts from unaccepted;
    /// resubmission requires operation-specific recovery semantics. Ids are never reused,
    /// so a late frame from the old channel cannot be mistaken for a new answer.
    pub fn replace_channel(&mut self, channel: C) {
        for trace in self.calls.values_mut() {
            trace.reopened();
        }
        self.channel = channel;
    }

    /// Mint an invocation id. Ids are never reused, including across
    /// `replace_channel`, so a late frame from an abandoned connection cannot be
    /// mistaken for the answer to a newer invocation.
    pub fn next_id(&mut self) -> Result<u64, Error> {
        self.sequence.allocate()
    }

    pub fn outstanding(&self) -> usize {
        self.calls.len()
    }

    /// Stop tracking an unresolved invocation locally. This sends nothing and
    /// does not cancel accepted server work or establish whether it committed.
    /// Late frames for this id are discarded. The allocator is unchanged, so
    /// abandoning a call never authorizes its automatic replay or reuses its id.
    pub fn abandon(&mut self, id: u64) -> bool {
        self.calls.remove(&id).is_some()
    }

    /// Send an invocation on the attached logical connection and await its
    /// outcome, for callers that want a return value rather than a stream.
    pub async fn invoke(&mut self, operation: &str, input: Value) -> Result<Value, Error> {
        let id = self.begin(operation, input).await?;
        self.await_outcome(id, |_| {}).await
    }

    /// Queue an invocation on the attached logical connection and return its id.
    ///
    /// Returns once the carrier has the command, not once the operation is
    /// accepted or finished. Observations arrive later, one per frame, through
    /// [`Self::pump`] — acceptance is deliberately publishable before the handler
    /// runs so a slow operation cannot look hung. Use this when the caller wants
    /// the id, or wants progress before the outcome.
    pub async fn begin(&mut self, operation: &str, input: Value) -> Result<u64, Error> {
        let id = self.next_id()?;
        self.track(Command::Invoke(invocation(id, operation, input)))
            .await
    }

    /// Send a connectionless invocation and await its outcome. Resolves a
    /// credential per call and establishes no logical connection.
    pub async fn request(
        &mut self,
        bearer: Option<&str>,
        operation: &str,
        input: Value,
    ) -> Result<Value, Error> {
        let id = self.begin_request(bearer, operation, input).await?;
        self.await_outcome(id, |_| {}).await
    }

    /// Queue a connectionless invocation and return its id, for callers that want
    /// the id or want progress before the outcome. Await with
    /// [`Self::await_outcome`].
    pub async fn begin_request(
        &mut self,
        bearer: Option<&str>,
        operation: &str,
        input: Value,
    ) -> Result<u64, Error> {
        let id = self.next_id()?;
        let bearer = bearer.map(String::from);
        self.track(Command::Request {
            bearer,
            invocation: invocation(id, operation, input),
        })
        .await
    }

    /// Send and await in one step, for callers that do not need the id or any
    /// intermediate progress. Equivalent to `begin_request` then `await_outcome`.
    pub async fn call(
        &mut self,
        bearer: Option<&str>,
        operation: &str,
        input: Value,
    ) -> Result<Value, Error> {
        let id = self.begin_request(bearer, operation, input).await?;
        self.await_outcome(id, |_| {}).await
    }

    async fn track(&mut self, command: Command) -> Result<u64, Error> {
        let (Command::Request { invocation, .. } | Command::Invoke(invocation)) = &command else {
            return Err(Error::Protocol);
        };
        let id = invocation.id;
        self.calls.insert(id, Trace::new(id, 0));
        self.channel.send(command).await?;
        Ok(id)
    }

    /// Establish a logical connection, or reattach to one retained after a lost
    /// socket. Returns whether retained state was resumed.
    ///
    /// This is the only point where a bearer is exchanged. Afterwards the socket
    /// is channelled into the logical connection and invocations carry no
    /// credential.
    pub async fn connect(&mut self, bearer: &str, client_id: &str) -> Result<bool, Error> {
        let command = Command::Connect {
            bearer: bearer.into(),
            client_id: client_id.into(),
        };
        self.channel.send(command).await?;
        loop {
            match self.channel.receive().await? {
                Some(Response::Attached { resumed }) => return Ok(resumed),
                Some(Response::Failed(error)) => return Err(error),
                // A global push may already be arriving; it is not this call's
                // answer, and the handler table still owns it.
                Some(Response::Global { kind, input }) => self.dispatch_global(kind, input),
                Some(_) => return Err(Error::Protocol),
                None => return Err(Error::Unavailable),
            }
        }
    }

    /// Drop the physical attachment. The logical connection is retained for
    /// reconnect, so this is not an ending.
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        self.end(Command::Disconnect).await
    }

    /// Close the logical connection. Accepted work still finishes.
    pub async fn close(&mut self) -> Result<(), Error> {
        self.end(Command::Close).await
    }

    async fn end(&mut self, command: Command) -> Result<(), Error> {
        self.channel.send(command).await?;
        loop {
            match self.channel.receive().await? {
                Some(Response::Detached) => return Ok(()),
                Some(Response::Failed(error)) => return Err(error),
                Some(Response::Global { kind, input }) => self.dispatch_global(kind, input),
                Some(_) => return Err(Error::Protocol),
                None => return Err(Error::Unavailable),
            }
        }
    }

    fn dispatch_global(&mut self, kind: String, input: Value) {
        if let Some(handler) = self.handlers.get_mut(&kind) {
            handler(input);
        }
    }

    /// Await one invocation's terminal outcome, pumping the stream until it
    /// arrives.
    ///
    /// This is the synchronous-looking face of the stream: the caller sends, then
    /// blocks here while progress for *this* invocation is forwarded to
    /// `progress`. Frames for other invocations and global pushes are still
    /// routed correctly on the way through, so several callers may share one
    /// channel only if they do not await concurrently — awaiting borrows the
    /// client exclusively, which is what keeps one caller's pump from consuming
    /// another's completion.
    ///
    /// A closed channel or an uncorrelated refusal is `Unavailable`: the
    /// operation's outcome is genuinely unknown, and reporting anything stronger
    /// would invite a caller to retry a mutation that may have committed.
    pub async fn await_outcome(
        &mut self,
        id: u64,
        mut progress: impl FnMut(Value),
    ) -> Result<Value, Error> {
        loop {
            match self.pump().await? {
                Pump::Completed {
                    id: completed,
                    outcome,
                } if completed == id => return outcome,
                Pump::Progress {
                    id: progressed,
                    value,
                } if progressed == id => progress(value),
                Pump::Closed => {
                    self.calls.remove(&id);
                    return Err(Error::Unavailable);
                }
                Pump::Refused(error) => return Err(error),
                // This invocation's own trace was abandoned, so no later frame
                // can resolve it. Reporting the break now is better than
                // waiting on a stream that will never answer it — which would
                // turn a known-lost call into an "unknown outcome" that only
                // resolves when the socket dies.
                Pump::Discarded(Some(lost)) if lost == id => {
                    self.calls.remove(&id);
                    return Err(Error::Protocol);
                }
                // Another invocation's frames, an acceptance, or a discarded
                // frame belonging to nobody: keep waiting for the one asked for.
                _ => {}
            }
        }
    }

    /// Consume one frame and route it.
    ///
    /// A frame this client cannot place is discarded rather than reported: a peer
    /// controls what it sends, so an unroutable frame is not the client's error.
    /// In particular a `Global` frame with no registered handler is dropped, so an
    /// unhandled topic cannot become a client-side failure or a log flood.
    pub async fn pump(&mut self) -> Result<Pump, Error> {
        let Some(response) = self.channel.receive().await? else {
            self.calls.clear();
            return Ok(Pump::Closed);
        };
        match response {
            Response::Global { kind, input } => {
                // An unhandled kind is dropped, not reported: a peer can publish
                // frames nobody subscribed to, so surfacing them would let any
                // server turn an unknown topic into a client-side failure.
                if self.handlers.contains_key(&kind) {
                    self.dispatch_global(kind, input);
                    return Ok(Pump::Global);
                }
                Ok(Pump::Discarded(None))
            }
            Response::Event(event) => {
                let id = event_id(&event);
                let Some(trace) = self.calls.get_mut(&id) else {
                    return Ok(Pump::Discarded(None));
                };
                let observation = match trace.receive(event) {
                    Ok(observation) => observation,
                    // This trace no longer describes the wire. Abandon it, so a
                    // later frame for the same id cannot resume a broken sequence.
                    Err(_) => {
                        self.calls.remove(&id);
                        return Ok(Pump::Discarded(Some(id)));
                    }
                };
                match observation {
                    Observation::Accepted => Ok(Pump::Accepted { id }),
                    Observation::Progress(value) => Ok(Pump::Progress { id, value }),
                    Observation::Bearer(change) => {
                        trace.bearer = Some(change);
                        Ok(Pump::Accepted { id })
                    }
                    Observation::Completed(outcome) => {
                        let change = trace.bearer.take();
                        self.calls.remove(&id);
                        // A failed operation must not leave a credential behind.
                        if outcome.is_err() {
                            return Ok(match change {
                                // A credential arrived for an operation that
                                // then failed. The trace is abandoned so the
                                // unusable change is never installed.
                                Some(_) => Pump::Discarded(Some(id)),
                                None => Pump::Completed { id, outcome },
                            });
                        }
                        if let Some(change) = change {
                            self.bearer = match change {
                                crate::bearer::Change::Set(token) => Some(token),
                                crate::bearer::Change::Clear => None,
                            };
                        }
                        Ok(Pump::Completed { id, outcome })
                    }
                }
            }
            // Refusal is uncorrelated: the invocation never began, so no id owns
            // it. Outstanding work cannot be resolved by it and is abandoned.
            Response::Failed(error) => {
                self.calls.clear();
                Ok(Pump::Refused(error))
            }
            Response::Attached { .. } | Response::Detached => Ok(Pump::Discarded(None)),
        }
    }
}

/// One invocation's correlation trace. ACK disables the caller-supplied acceptance
/// timer. Progress is transient and completion is terminal. The timer does not
/// authorize retransmission: only an operation's own recovery contract can do so.
pub struct Trace {
    id: u64,
    retry_at: u64,
    accepted: bool,
    completed: bool,
    /// Credential change observed before completion. Applied only if the
    /// operation then succeeds; a failed operation must not leave a new bearer.
    pub(crate) bearer: Option<crate::bearer::Change>,
}

#[derive(Debug, PartialEq)]
pub enum Observation {
    Accepted,
    Progress(Value),
    Completed(Outcome),
    Bearer(crate::bearer::Change),
}

impl Trace {
    pub fn new(id: u64, retry_at: u64) -> Self {
        Self {
            id,
            retry_at,
            accepted: false,
            completed: false,
            bearer: None,
        }
    }

    pub fn retry_due(&self, now: u64) -> bool {
        !self.accepted && !self.completed && now >= self.retry_at
    }

    pub fn retried(&mut self, retry_at: u64) {
        self.retry_at = retry_at;
    }

    /// Reset to pre-acceptance after the physical attachment is replaced.
    ///
    /// Completion is deliberately not cleared, so a terminated trace cannot be
    /// revived. Reopening correlation state does not recover a lost result or
    /// authorize resubmission of an operation with an unknown outcome.
    pub(crate) fn reopened(&mut self) {
        self.accepted = false;
        self.bearer = None;
        self.retry_at = 0;
    }

    pub fn receive(&mut self, event: Event) -> Result<Observation, Error> {
        if self.completed {
            return Err(Error::Protocol);
        }
        match event {
            Event::Accepted { id } if id == self.id => {
                self.accepted = true;
                Ok(Observation::Accepted)
            }
            Event::Bearer { id, change } if id == self.id && self.accepted => {
                Ok(Observation::Bearer(change))
            }
            Event::Progress { id, value } if id == self.id && self.accepted => {
                Ok(Observation::Progress(value))
            }
            Event::Completed { id, outcome }
                if id == self.id && (self.accepted || outcome.is_err()) =>
            {
                self.completed = true;
                Ok(Observation::Completed(outcome))
            }
            _ => Err(Error::Protocol),
        }
    }
}

/// Build an invocation for `id`. The operation name is validated by the server's
/// registry; an unknown one is refused before any handler runs.
fn invocation(id: u64, operation: &str, input: Value) -> Invocation {
    Invocation {
        id,
        operation: operation.into(),
        input,
    }
}

fn event_id(event: &Event) -> u64 {
    match event {
        Event::Accepted { id }
        | Event::Bearer { id, .. }
        | Event::Progress { id, .. }
        | Event::Completed { id, .. } => *id,
    }
}

pub fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::InvalidOutput)
}
