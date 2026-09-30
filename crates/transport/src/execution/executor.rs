use super::{Admission, Attempt, Call, Error, Inputs, Outcome, Program, Value, View, WorkingSet};
use crate::dispatch::Queue;
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Scope(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ticket(u64);
impl Ticket {
    /// Host observation ID, never an authority to submit or resume work.
    pub fn id(self) -> u64 {
        self.0
    }
}

/// Read-only host inspection. Values describe committed state and queued inputs;
/// an attempt's scratch data never survives long enough to appear here.
pub struct Inspection<'a> {
    pub states: &'a BTreeMap<Scope, Value>,
    pub active: Option<JobView<'a>>,
    pub queued: Vec<JobView<'a>>,
    pub releases: usize,
    pub paused: bool,
}
pub struct JobView<'a> {
    pub ticket: Ticket,
    pub scope: Option<Scope>,
    pub call: &'a Call,
    pub accepted: bool,
    pub waiting: Option<&'a str>,
    pub inputs: &'a BTreeMap<String, Value>,
}
impl Job {
    fn view(&self) -> JobView<'_> {
        JobView {
            ticket: self.ticket,
            scope: self.scope,
            call: &self.call,
            accepted: self.accepted,
            waiting: self.waiting.as_deref(),
            inputs: &self.inputs,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// The host owns this FIFO slot until it calls `finish_reserved`. No program
    /// code or credential-bearing request data enters this slot.
    Reserved(Ticket),
    Accepted(Ticket),
    Need {
        ticket: Ticket,
        key: String,
    },
    Completed {
        ticket: Ticket,
        outcome: Outcome,
    },
}

struct Job {
    ticket: Ticket,
    scope: Option<Scope>,
    call: Call,
    inputs: BTreeMap<String, Value>,
    accepted: bool,
    waiting: Option<String>,
    failure: Option<Error>,
    definition: Option<usize>,
}
enum Queued {
    Run(Job),
    Release(Scope),
    Reserved(Ticket),
}

/// One application-wide gate. The active job retains ownership while waiting for
/// a dependency. All other operations and state-release actions stay in the FIFO.
/// ACK is a separate step before handler entry. Completion follows validation and
/// atomic in-memory publication; output delivery is the host's responsibility.
pub struct Executor<P: Program> {
    program: P,
    states: BTreeMap<Scope, Value>,
    closing: BTreeSet<Scope>,
    queue: Queue<Queued>,
    active: Option<Job>,
    reserved: Option<Ticket>,
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
            queue: Queue::default(),
            active: None,
            reserved: None,
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
    /// Retire unaccepted work immediately. An accepted operation retains its scope
    /// through completion, including held dependencies; release follows it.
    pub fn discard(&mut self, scope: Scope) {
        let draining = self
            .active
            .as_ref()
            .is_some_and(|job| job.scope == Some(scope) && job.accepted);
        if !draining {
            self.states.remove(&scope);
            self.closing.remove(&scope);
        }
        self.queue
            .retain(|entry| !matches!(entry, Queued::Release(id) if *id == scope));
        let fail = |job: &mut Job| {
            if job.scope == Some(scope) && !job.accepted {
                job.waiting = None;
                job.failure = Some(Error::IdentityRequired);
            }
        };
        if let Some(job) = &mut self.active {
            fail(job);
        }
        for entry in self.queue.iter_mut() {
            if let Queued::Run(job) = entry {
                fail(job);
            }
        }
        if draining {
            self.closing.remove(&scope);
            self.release(scope);
        }
    }
    pub fn inspect(&self) -> Inspection<'_> {
        Inspection {
            states: &self.states,
            active: self.active.as_ref().map(Job::view),
            queued: self
                .queue
                .iter()
                .filter_map(|entry| match entry {
                    Queued::Run(job) => Some(job.view()),
                    Queued::Release(_) | Queued::Reserved(_) => None,
                })
                .collect(),
            releases: self
                .queue
                .iter()
                .filter(|entry| matches!(entry, Queued::Release(_)))
                .count(),
            paused: self.paused,
        }
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
        if self.queue.len() + usize::from(self.active.is_some() || self.reserved.is_some())
            >= self.capacity
        {
            return Err(Error::Capacity);
        }
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        let ticket = Ticket(self.sequence);
        self.queue.push_back(Queued::Run(Job {
            ticket,
            scope,
            definition: self
                .program
                .operations()
                .iter()
                .position(|op| op.key == call.operation),
            call,
            inputs: BTreeMap::new(),
            accepted: false,
            waiting: None,
            failure: None,
        }));
        Ok(ticket)
    }

    /// Reserve a FIFO operation slot for a host-composed transactional capability.
    /// Admission starts only when `step` emits Reserved. The host queues its ACK
    /// after its guard succeeds and releases this slot after its durable completion.
    pub fn reserve(&mut self) -> Result<Ticket, Error> {
        if self.paused {
            return Err(Error::Unavailable);
        }
        if self.queue.len() + usize::from(self.active.is_some() || self.reserved.is_some())
            >= self.capacity
        {
            return Err(Error::Capacity);
        }
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        let ticket = Ticket(self.sequence);
        self.queue.push_back(Queued::Reserved(ticket));
        Ok(ticket)
    }

    pub fn finish_reserved(&mut self, ticket: Ticket) -> Result<(), Error> {
        if self.reserved != Some(ticket) {
            return Err(Error::Protocol);
        }
        self.reserved = None;
        self.queue.finish();
        Ok(())
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
        if self.reserved.is_some() {
            return None;
        }
        // Discarded queued work is already terminal. Deliver its failure even
        // while another scope owns the application gate and waits for input.
        if let Some(index) = self
            .queue
            .iter()
            .position(|entry| matches!(entry, Queued::Run(job) if job.failure.is_some()))
        {
            let Queued::Run(job) = self.queue.remove(index).unwrap() else {
                unreachable!()
            };
            return Some(Event::Completed {
                ticket: job.ticket,
                outcome: Err(self.checked_error(job.failure.unwrap())),
            });
        }
        while self.active.is_none() {
            match self.queue.acquire()? {
                Queued::Run(job) => self.active = Some(job),
                Queued::Release(scope) => {
                    self.states.remove(&scope);
                    self.closing.remove(&scope);
                    self.queue.finish();
                }
                Queued::Reserved(ticket) => {
                    self.reserved = Some(ticket);
                    return Some(Event::Reserved(ticket));
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
        let Some(definition) = job.definition else {
            return Some(self.complete(Err(Error::UnknownOperation)));
        };
        let op = &self.program.operations()[definition];
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
            let op = &self.program.operations()[job.definition.expect("selected operation")];
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
        self.queue.finish();
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
        self.queue.idle()
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
