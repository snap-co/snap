//! Development execution controls shared by HTTP tools and the browser panel.
//! Manual mode stops host stepping, not submissions. A step is one executor
//! observation, never a source-line breakpoint. Snapshots require an idle gate.
use serde::Deserialize;
use snap_transport::execution;
use snap_transport::execution::{Call, JobView, Program, Snapshot, Ticket};
use snap_transport::execution::{Observation, Peer, Runtime, Submission};
use snap_transport::{Command, Event, Response, Value, json, server::Authority};
use std::collections::{BTreeMap, VecDeque};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Open,
    Send {
        peer: u64,
        command: Command,
    },
    Drop {
        peer: u64,
    },
    Drain {
        peer: u64,
    },
    Mode {
        manual: bool,
    },
    Breakpoint {
        enabled: bool,
    },
    Step,
    Supply {
        ticket: u64,
        key: String,
        value: Value,
    },
    Fail {
        ticket: u64,
        key: String,
    },
    Snapshot,
    Restore,
    Replace {
        program: String,
    },
}
struct Connection {
    peer: Peer,
    pending: bool,
    responses: VecDeque<Response>,
}
pub struct Development<P: Program, R: Authority> {
    platform: Runtime<P, R>,
    peers: BTreeMap<u64, Connection>,
    owners: BTreeMap<Ticket, u64>,
    sequence: u64,
    manual: bool,
    breakpoint: bool,
    trace: VecDeque<Value>,
    trace_sequence: u64,
    snapshot: Option<Snapshot>,
    reads: fn(&Call, &str) -> execution::Outcome,
    programs: fn(&str) -> Option<P>,
    program: String,
}
impl<P: Program, R: Authority> Development<P, R> {
    pub fn new(
        platform: Runtime<P, R>,
        reads: fn(&Call, &str) -> execution::Outcome,
        programs: fn(&str) -> Option<P>,
    ) -> Self {
        Self {
            platform,
            peers: BTreeMap::new(),
            owners: BTreeMap::new(),
            sequence: 0,
            manual: false,
            breakpoint: false,
            trace: VecDeque::new(),
            trace_sequence: 0,
            snapshot: None,
            reads,
            programs,
            program: "standard".into(),
        }
    }
    fn record(&mut self, event: Value) {
        self.trace_sequence += 1;
        if self.trace.len() == 256 {
            self.trace.pop_front();
        }
        self.trace
            .push_back(json!({"sequence": self.trace_sequence, "event": event}));
    }
    pub fn open(&mut self) -> Result<u64, String> {
        if self.peers.len() >= 128 {
            return Err("peer capacity reached".into());
        }
        self.sequence = self.sequence.checked_add(1).ok_or("peer ID exhausted")?;
        self.peers.insert(
            self.sequence,
            Connection {
                peer: Peer::default(),
                pending: false,
                responses: VecDeque::new(),
            },
        );
        Ok(self.sequence)
    }
    pub fn lost(&mut self, id: u64, now: u64) {
        if let Some(mut connection) = self.peers.remove(&id) {
            self.platform.lost(&mut connection.peer, now);
        }
        self.pump();
    }
    /// Physical teardown bypasses the outstanding-command guard. Logical Close
    /// must still reach Transport while accepted work is waiting for a host input.
    pub(crate) fn teardown(&mut self, id: u64, close: bool, now: u64) -> Response {
        let response = if let Some(connection) = self.peers.get_mut(&id) {
            let command = if close {
                Command::Close
            } else {
                Command::Disconnect
            };
            match self.platform.submit(&mut connection.peer, command, now) {
                Submission::Ready(response) => response,
                Submission::Pending(_) => unreachable!("teardown never invokes an operation"),
            }
        } else {
            Response::Failed(snap_transport::Error::StaleConnection)
        };
        self.lost(id, now);
        response
    }
    pub fn send(&mut self, id: u64, command: Command, now: u64) -> Result<(), String> {
        let connection = self.peers.get_mut(&id).ok_or("unknown peer")?;
        if connection.pending {
            return Err("peer already has an outstanding command".into());
        }
        if connection.responses.len() >= 64 {
            return Err("drain peer responses first".into());
        }
        let invocation = match &command {
            Command::Invoke(invocation) | Command::Request { invocation, .. } => {
                Some(invocation.clone())
            }
            _ => None,
        };
        match self.platform.submit(&mut connection.peer, command, now) {
            Submission::Ready(Response::Events(events)) => {
                for event in events {
                    connection
                        .responses
                        .push_back(Response::Events(vec![event]));
                }
            }
            Submission::Ready(response) => connection.responses.push_back(response),
            Submission::Pending(ticket) => {
                connection.pending = true;
                self.owners.insert(ticket, id);
                if !self.platform.private_request(ticket) {
                    self.record(
                        json!({"submitted": ticket.id(), "peer": id, "invocation": invocation}),
                    );
                }
            }
        }
        self.pump();
        Ok(())
    }
    pub fn drain(&mut self, id: u64) -> Result<Vec<Response>, String> {
        Ok(self
            .peers
            .get_mut(&id)
            .ok_or("unknown peer")?
            .responses
            .drain(..)
            .collect())
    }
    pub fn retired(&self, id: u64) -> bool {
        self.peers
            .get(&id)
            .is_none_or(|connection| self.platform.retired(&connection.peer))
    }
    /// Whether inspection may have changed. Idle sweeps need no report encoding.
    pub fn tick(&mut self, now: u64) -> bool {
        let expired = self.platform.tick(now);
        let before = self.trace_sequence;
        self.pump();
        expired || before != self.trace_sequence
    }
    fn step(&mut self) -> bool {
        let Some(observation) = self.platform.step() else {
            return false;
        };
        match observation {
            Observation::Event {
                ticket,
                event,
                private,
            } => {
                let completed = matches!(event, Event::Completed { .. });
                let accepted = matches!(event, Event::Accepted { .. });
                let owner = self.owners[&ticket];
                if !private {
                    self.record(
                        json!({"ticket": ticket.id(), "peer": owner, "observation": event}),
                    );
                }
                if let Some(connection) = self.peers.get_mut(&owner) {
                    connection
                        .responses
                        .push_back(Response::Events(vec![event]));
                    if completed {
                        connection.pending = false;
                    }
                }
                if completed {
                    self.owners.remove(&ticket);
                }
                if accepted && !private && self.breakpoint {
                    self.manual = true;
                }
            }
            Observation::Need { ticket, key } => {
                self.record(json!({"ticket": ticket.id(), "need": key}))
            }
        }
        true
    }
    fn pump(&mut self) {
        while !self.manual {
            let waiting = self
                .platform
                .inspect()
                .active
                .and_then(|job| job.waiting.map(|key| (job.ticket, key.to_owned())));
            if let Some((ticket, key)) = waiting {
                let result = (self.reads)(self.platform.pending_call(ticket).unwrap(), &key);
                self.record_input(ticket.id(), &key, &result);
                self.platform
                    .supply(ticket, &key, result)
                    .expect("current read");
            }
            if !self.step() {
                break;
            }
        }
    }
    fn supply(&mut self, id: u64, key: &str, value: execution::Outcome) -> Result<(), String> {
        let ticket = self
            .platform
            .inspect()
            .active
            .filter(|job| job.ticket.id() == id)
            .map(|job| job.ticket)
            .ok_or("stale ticket")?;
        self.platform
            .supply(ticket, key, value.clone())
            .map_err(|error| format!("{error:?}"))?;
        self.record_input(id, key, &value);
        self.pump();
        Ok(())
    }
    fn record_input(&mut self, id: u64, key: &str, value: &execution::Outcome) {
        let outcome = match value {
            Ok(value) => json!({"Ok": value}),
            Err(error) => json!({"Err": format!("{error:?}")}),
        };
        self.record(json!({"supplied": id, "key": key, "outcome": outcome}));
    }
    pub fn control(&mut self, control: Control, now: u64) -> Result<Value, String> {
        match control {
            Control::Open => return Ok(json!({"peer": self.open()?})),
            Control::Send { peer, command } => self.send(peer, command, now)?,
            Control::Drop { peer } => self.lost(peer, now),
            Control::Drain { peer } => return Ok(json!({"responses": self.drain(peer)?})),
            Control::Mode { manual } => {
                self.manual = manual;
                self.pump();
            }
            Control::Breakpoint { enabled } => self.breakpoint = enabled,
            Control::Step => {
                self.manual = true;
                self.step();
            }
            Control::Supply { ticket, key, value } => self.supply(ticket, &key, Ok(value))?,
            Control::Fail { ticket, key } => {
                self.supply(ticket, &key, Err(execution::Error::Unavailable))?
            }
            Control::Snapshot => {
                self.snapshot = Some(
                    self.platform
                        .snapshot()
                        .map_err(|error| format!("{error:?}"))?,
                );
                self.record(json!({"snapshot": "saved"}));
            }
            Control::Restore => {
                let snapshot = self.snapshot.as_ref().ok_or("no saved snapshot")?;
                self.platform.pause();
                let result = self.platform.restore(snapshot);
                self.platform.resume();
                result.map_err(|error| format!("{error:?}"))?;
                self.record(json!({"snapshot": "restored"}));
            }
            Control::Replace { program } => {
                let replacement = (self.programs)(&program).ok_or("unknown program variant")?;
                self.platform.pause();
                let result = self.platform.replace(replacement);
                self.platform.resume();
                result.map_err(|error| format!("{error:?}"))?;
                self.program = program;
                self.record(json!({"replaced": self.program}));
            }
        }
        Ok(self.inspect())
    }
    pub fn inspect(&self) -> Value {
        fn job(view: JobView<'_>) -> Value {
            json!({"ticket": view.ticket.id(), "scope": view.scope.map(|s| s.0),
                "operation": view.call.operation, "input": view.call.input,
                "accepted": view.accepted, "waiting": view.waiting, "inputs": view.inputs})
        }
        let view = self.platform.inspect();
        json!({"manual": self.manual, "breakpoint": self.breakpoint, "program": self.program,
            "snapshot": self.snapshot.is_some(), "active": view.active.map(job),
            "queued": view.queued.into_iter().map(job).collect::<Vec<_>>(), "releases": view.releases,
            "states": view.states.iter().map(|(scope, state)| json!({"scope": scope.0, "state": state})).collect::<Vec<_>>(),
            "peers": self.peers.iter().map(|(id, connection)| json!({"id": id, "pending": connection.pending})).collect::<Vec<_>>(),
            "trace": self.trace})
    }
}
