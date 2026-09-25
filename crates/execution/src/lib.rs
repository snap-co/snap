//! IO-free application execution, serialized across a whole application instance.
//! The host drives `Executor::step` and supplies requested inputs. No application
//! stack, future, callback, or mutable reference survives an entry-point return.
#![no_std]
extern crate alloc;

use alloc::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    string::String,
};
pub use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    UnknownOperation,
    IdentityRequired,
    InvalidInput,
    InvalidOutput,
    InvalidState,
    Unavailable,
    Protocol,
    Capacity,
    Application(Value),
}
pub type Outcome = Result<Value, Error>;
pub type Validator = fn(&Value) -> bool;

/// Descriptions belong to the selected program. The executor never caches these
/// function pointers across program replacement.
pub struct Operation {
    pub key: &'static str,
    pub identity_required: bool,
    pub input: Validator,
    pub output: Validator,
    pub error: Validator,
}

/// Constructed by trusted host composition, never deserialized from a caller.
#[derive(Clone, Debug)]
pub struct Call {
    pub operation: String,
    pub input: Value,
    pub identity: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Scope(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ticket(u64);

pub enum Admission {
    Ready,
    Need(String),
    Reject(Error),
}
pub enum Attempt {
    Need(String),
    Commit { state: Value, result: Value },
    Fail(Error),
}
pub enum Stop {
    Need(String),
    Fail(Error),
}
impl From<Error> for Stop {
    fn from(error: Error) -> Self {
        Self::Fail(error)
    }
}

/// Local application helper. `read` consults supplied values only; it makes no
/// host call. A missing key requests a read-only dependency, never an external
/// side effect. The host authorizes/resolves that request in the call's context.
pub struct Inputs<'a> {
    values: &'a BTreeMap<String, Value>,
}
impl Inputs<'_> {
    pub fn read(&self, key: &str) -> Result<&Value, Stop> {
        self.values.get(key).ok_or_else(|| Stop::Need(key.into()))
    }
}
pub struct View<'a> {
    pub state: &'a Value,
    pub connected: bool,
    pub inputs: Inputs<'a>,
}

/// A deep copy of committed JSON data. There are no shared interior-mutable
/// pointers into live state. Drop on Need/Fail rolls back *all* tentative edits.
/// Later reads of `state` see this attempt's earlier writes.
pub struct WorkingSet<'a> {
    pub state: Value,
    pub connected: bool,
    pub inputs: Inputs<'a>,
}
impl WorkingSet<'_> {
    pub fn finish(self, result: Result<Value, Stop>) -> Attempt {
        match result {
            Ok(result) => Attempt::Commit {
                state: self.state,
                result,
            },
            Err(Stop::Need(key)) => Attempt::Need(key),
            Err(Stop::Fail(error)) => Attempt::Fail(error),
        }
    }
}

/// Application entry points. Implementations must keep no invocation state in
/// themselves, perform no IO, and treat supplied inputs as immutable. These are
/// ordinary Rust calls in this build, not a stable binary ABI or an IO sandbox.
/// A future module adapter must transfer owned data before resetting guest scratch.
pub trait Program {
    fn state_version(&self) -> u64;
    fn valid_state(&self, state: &Value) -> bool;
    fn operations(&self) -> &[Operation];
    fn admit(&self, call: &Call, view: View<'_>) -> Admission;
    fn attempt(&self, call: &Call, work: WorkingSet<'_>) -> Attempt;
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Accepted(Ticket),
    Need { ticket: Ticket, key: String },
    Completed { ticket: Ticket, outcome: Outcome },
}

struct Job {
    ticket: Ticket,
    scope: Option<Scope>,
    call: Call,
    inputs: BTreeMap<String, Value>,
    accepted: bool,
    waiting: Option<String>,
    failure: Option<Error>,
}
enum Queued {
    Run(Job),
    Release(Scope),
}

/// One application-wide gate. The active job retains ownership while waiting for
/// a dependency. All other operations and state-release actions stay in the FIFO.
/// ACK is a separate step before handler entry. Completion follows validation and
/// atomic in-memory publication; output delivery is the host's responsibility.
pub struct Executor<P: Program> {
    program: P,
    states: BTreeMap<Scope, Value>,
    closing: BTreeSet<Scope>,
    queue: VecDeque<Queued>,
    active: Option<Job>,
    sequence: u64,
    paused: bool,
    capacity: usize,
}

/// Data-only snapshot at an idle point. Transport, sockets, pending IO and clocks
/// are not captured. Restore requires the same set of live logical scopes.
#[derive(Clone)]
pub struct Snapshot {
    version: u64,
    states: BTreeMap<Scope, Value>,
}

impl<P: Program> Executor<P> {
    pub fn new(program: P, capacity: usize) -> Result<Self, Error> {
        Self::validate_program(&program)?;
        Ok(Self {
            program,
            states: BTreeMap::new(),
            closing: BTreeSet::new(),
            queue: VecDeque::new(),
            active: None,
            sequence: 0,
            paused: false,
            capacity,
        })
    }
    fn validate_program(program: &P) -> Result<(), Error> {
        if !program.valid_state(&Value::Null) {
            return Err(Error::InvalidState);
        }
        for (i, op) in program.operations().iter().enumerate() {
            if op.key.is_empty()
                || program.operations()[..i]
                    .iter()
                    .any(|other| other.key == op.key)
            {
                return Err(Error::Protocol);
            }
        }
        Ok(())
    }
    /// New scopes start uninitialized. Only host composition allocates scope IDs.
    pub fn open(&mut self, scope: Scope) -> Result<(), Error> {
        if self.states.contains_key(&scope) {
            return Err(Error::Protocol);
        }
        self.states.insert(scope, Value::Null);
        Ok(())
    }
    /// Release is ordered after all previously submitted work. Already queued work
    /// may finish, but further submissions to this scope fail immediately.
    pub fn release(&mut self, scope: Scope) {
        if self.states.contains_key(&scope) && self.closing.insert(scope) {
            self.queue.push_back(Queued::Release(scope));
        }
    }
    pub fn state(&self, scope: Scope) -> Option<&Value> {
        self.states.get(&scope)
    }
    pub fn submit(&mut self, scope: Option<Scope>, call: Call) -> Result<Ticket, Error> {
        if self.paused {
            return Err(Error::Unavailable);
        }
        if scope
            .is_some_and(|scope| !self.states.contains_key(&scope) || self.closing.contains(&scope))
        {
            return Err(Error::Protocol);
        }
        if self.queue.len() + usize::from(self.active.is_some()) >= self.capacity {
            return Err(Error::Capacity);
        }
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        let ticket = Ticket(self.sequence);
        self.queue.push_back(Queued::Run(Job {
            ticket,
            scope,
            call,
            inputs: BTreeMap::new(),
            accepted: false,
            waiting: None,
            failure: None,
        }));
        Ok(ticket)
    }
    /// Resolves exactly the outstanding read. Failure ends the operation without
    /// publication. Stale or mismatched responses cannot resume another operation.
    pub fn supply(&mut self, ticket: Ticket, key: &str, result: Outcome) -> Result<(), Error> {
        let job = self
            .active
            .as_mut()
            .filter(|job| job.ticket == ticket && job.waiting.as_deref() == Some(key))
            .ok_or(Error::Protocol)?;
        job.waiting = None;
        match result {
            Ok(value) => {
                job.inputs.insert(key.into(), value);
            }
            Err(error) => job.failure = Some(error),
        }
        Ok(())
    }
    /// Host-side dependency resolution can inspect the trusted call while waiting.
    pub fn pending_call(&self, ticket: Ticket) -> Option<&Call> {
        self.active
            .as_ref()
            .filter(|job| job.ticket == ticket && job.waiting.is_some())
            .map(|job| &job.call)
    }
    /// Performs one observable transition. None means idle or waiting on a read.
    /// Call again after Accepted to enter the handler, or after supply to retry.
    pub fn step(&mut self) -> Option<Event> {
        while self.active.is_none() {
            match self.queue.pop_front()? {
                Queued::Run(job) => self.active = Some(job),
                Queued::Release(scope) => {
                    self.states.remove(&scope);
                    self.closing.remove(&scope);
                }
            }
        }
        let job = self.active.as_ref().unwrap();
        if job.waiting.is_some() {
            return None;
        }
        let ticket = job.ticket;
        if let Some(error) = &job.failure {
            return Some(self.complete(Err(self.checked_error(error.clone()))));
        }
        let Some(op) = self
            .program
            .operations()
            .iter()
            .find(|op| op.key == job.call.operation)
        else {
            return Some(self.complete(Err(Error::UnknownOperation)));
        };
        let state = job
            .scope
            .map(|scope| &self.states[&scope])
            .unwrap_or(&Value::Null);
        if !job.accepted {
            let admission = if !(op.input)(&job.call.input) {
                Admission::Reject(Error::InvalidInput)
            } else if op.identity_required
                && job.call.identity.as_ref().is_none_or(|id| id.is_empty())
            {
                Admission::Reject(Error::IdentityRequired)
            } else {
                self.program.admit(
                    &job.call,
                    View {
                        state,
                        connected: job.scope.is_some(),
                        inputs: Inputs {
                            values: &job.inputs,
                        },
                    },
                )
            };
            return Some(match admission {
                Admission::Ready => {
                    self.active.as_mut().unwrap().accepted = true;
                    Event::Accepted(ticket)
                }
                Admission::Need(key) => self.need(key),
                Admission::Reject(error) => self.complete(Err(self.checked_error(error))),
            });
        }
        let work = WorkingSet {
            state: state.clone(),
            connected: job.scope.is_some(),
            inputs: Inputs {
                values: &job.inputs,
            },
        };
        let outcome = self.program.attempt(&job.call, work);
        Some(match outcome {
            Attempt::Need(key) => self.need(key),
            Attempt::Fail(error) => self.complete(Err(self.checked_error(error))),
            Attempt::Commit { state, result } => {
                if !(op.output)(&result) {
                    self.complete(Err(Error::InvalidOutput))
                } else if !self.program.valid_state(&state) {
                    self.complete(Err(Error::InvalidState))
                } else {
                    if let Some(scope) = self.active.as_ref().unwrap().scope {
                        self.states.insert(scope, state);
                    }
                    self.complete(Ok(result))
                }
            }
        })
    }
    fn checked_error(&self, error: Error) -> Error {
        if let Error::Application(value) = &error {
            let job = self.active.as_ref().unwrap();
            let op = self
                .program
                .operations()
                .iter()
                .find(|op| op.key == job.call.operation)
                .unwrap();
            if !(op.error)(value) {
                return Error::InvalidOutput;
            }
        }
        error
    }
    fn need(&mut self, key: String) -> Event {
        let job = self.active.as_mut().unwrap();
        // A supplied key must be consumed, not requested forever. Bound dynamic
        // discovery so a broken program cannot indefinitely monopolize the gate.
        if key.is_empty() || job.inputs.contains_key(&key) {
            return self.complete(Err(Error::Protocol));
        }
        if job.inputs.len() >= 64 {
            return self.complete(Err(Error::Capacity));
        }
        job.waiting = Some(key.clone());
        Event::Need {
            ticket: job.ticket,
            key,
        }
    }
    fn complete(&mut self, outcome: Outcome) -> Event {
        Event::Completed {
            ticket: self.active.take().unwrap().ticket,
            outcome,
        }
    }
    /// Stop submissions; already queued/admitted operations continue draining.
    /// A missing dependency still needs supply or an explicit failure from the host.
    pub fn pause(&mut self) {
        self.paused = true;
    }
    pub fn resume(&mut self) {
        self.paused = false;
    }
    pub fn idle(&self) -> bool {
        self.active.is_none() && self.queue.is_empty()
    }
    /// Code-only replacement at a drained gate. A rejected replacement preserves
    /// the old program and all state. The version is an application compatibility
    /// declaration, not proof that arbitrary Rust layouts are binary-compatible.
    pub fn replace(&mut self, program: P) -> Result<(), Error> {
        if !self.paused || !self.idle() {
            return Err(Error::Unavailable);
        }
        Self::validate_program(&program)?;
        if program.state_version() != self.program.state_version()
            || self
                .states
                .values()
                .any(|state| !program.valid_state(state))
        {
            return Err(Error::InvalidState);
        }
        self.program = program;
        Ok(())
    }
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        if !self.idle() {
            return Err(Error::Unavailable);
        }
        Ok(Snapshot {
            version: self.program.state_version(),
            states: self.states.clone(),
        })
    }
    pub fn restore(&mut self, snapshot: &Snapshot) -> Result<(), Error> {
        if !self.paused || !self.idle() {
            return Err(Error::Unavailable);
        }
        if snapshot.version != self.program.state_version()
            || !self.states.keys().eq(snapshot.states.keys())
            || snapshot
                .states
                .values()
                .any(|state| !self.program.valid_state(state))
        {
            return Err(Error::InvalidState);
        }
        self.states = snapshot.states.clone();
        Ok(())
    }
}
