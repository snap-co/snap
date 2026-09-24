//! Resident identity client shared by browser and native hosts.
//!
//! Commands are never replayed. A dropped connection rejects unfinished reads;
//! reconnect opens a new epoch and explicitly refreshes the server's snapshots.
//! Generation fencing discards late HTTP/socket observations after session change.
//! Host clocks, IO, timers and task cancellation do not enter this module.
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use serde::Serialize;
use snap_protocol::{Error, Invocation, Lane, Outcome, Value};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub phase: &'static str,
    pub identity_id: Option<String>,
    pub connection: &'static str,
    pub credentials: Vec<Value>,
    pub sessions: Vec<Value>,
    pub pending: bool,
    pub error: Option<Error>,
}

pub enum Input {
    Start,
    Command {
        id: u64,
        key: String,
        payload: Option<Value>,
    },
    Http {
        generation: u64,
        id: String,
        result: Result<String, Error>,
    },
    BuildMismatch {
        generation: u64,
    },
    Frame {
        generation: u64,
        wire: String,
    },
    Disconnected {
        generation: u64,
        code: u16,
    },
    Retry {
        generation: u64,
    },
    ReadTimeout {
        generation: u64,
        id: String,
    },
    Close,
}

pub enum Action {
    Http {
        generation: u64,
        invocation: Invocation,
        lane: Lane,
    },
    Connect {
        generation: u64,
    },
    Send {
        generation: u64,
        invocation: Invocation,
    },
    Disconnect,
    Retry {
        generation: u64,
        milliseconds: u32,
    },
    ReadDeadline {
        generation: u64,
        id: String,
    },
    Reload,
    Complete {
        id: u64,
        outcome: Outcome,
    },
}

struct Pending {
    key: String,
    command: Option<u64>,
}

pub struct Client {
    register: &'static str,
    state: Snapshot,
    generation: u64,
    socket_generation: u64,
    sequence: u64,
    message_sequence: u64,
    epoch: Option<String>,
    pending: BTreeMap<String, Pending>,
    retry: u32,
    closed: bool,
    session_ended: bool,
}

impl Client {
    pub fn new(register: &'static str) -> Self {
        Self {
            register,
            state: Snapshot {
                phase: "loading",
                identity_id: None,
                connection: "disconnected",
                credentials: Vec::new(),
                sessions: Vec::new(),
                pending: false,
                error: None,
            },
            generation: 0,
            socket_generation: 0,
            sequence: 0,
            message_sequence: 0,
            epoch: None,
            pending: BTreeMap::new(),
            retry: 250,
            closed: false,
            session_ended: false,
        }
    }
}

impl Client {
    pub fn snapshot(&self) -> Snapshot {
        self.state.clone()
    }

    fn http(
        &mut self,
        key: &str,
        payload: Option<Value>,
        command: Option<u64>,
        out: &mut Vec<Action>,
    ) {
        self.sequence += 1;
        let id = format!("identity-{}", self.sequence);
        self.pending.insert(
            id.clone(),
            Pending {
                key: key.into(),
                command,
            },
        );
        out.push(Action::Http {
            generation: self.generation,
            invocation: Invocation {
                operation_id: id,
                key: key.into(),
                payload,
                traceparent: None,
            },
            lane: if key == "identity.fetch" {
                Lane::Query
            } else {
                Lane::Submit
            },
        });
    }

    fn read(&mut self, key: &str, out: &mut Vec<Action>) {
        // Several renderers or push notifications may request the same refresh.
        // One resident read supplies them all; the controller does not queue copies.
        if self.pending.values().any(|p| p.key == key) {
            return;
        }
        let Some(epoch) = &self.epoch else { return };
        self.message_sequence += 1;
        let id = format!("{epoch}:{}", self.message_sequence);
        self.pending.insert(
            id.clone(),
            Pending {
                key: key.into(),
                command: None,
            },
        );
        out.push(Action::ReadDeadline {
            generation: self.socket_generation,
            id: id.clone(),
        });
        out.push(Action::Send {
            generation: self.socket_generation,
            invocation: Invocation {
                operation_id: id,
                key: key.into(),
                payload: None,
                traceparent: None,
            },
        });
    }

    fn reset(&mut self, out: &mut Vec<Action>) {
        self.session_ended = false;
        self.generation += 1;
        self.socket_generation += 1;
        self.epoch = None;
        self.state.connection = "disconnected";
        self.state.phase = "loading";
        self.state.identity_id = None;
        self.state.credentials.clear();
        self.state.sessions.clear();
        self.state.pending = false;
        for (_, p) in core::mem::take(&mut self.pending) {
            if let Some(id) = p.command {
                out.push(Action::Complete {
                    id,
                    outcome: Err(unavailable("Session changed")),
                });
            }
        }
        out.push(Action::Disconnect);
    }

    pub fn update(&mut self, input: Input, out: &mut Vec<Action>) {
        if self.closed {
            if let Input::Command { id, .. } = input {
                out.push(Action::Complete {
                    id,
                    outcome: Err(unavailable("Client is closed")),
                });
            }
            return;
        }
        match input {
            Input::Start => self.http("identity.fetch", None, None, out),
            Input::Command { id, key, payload } => {
                if self.state.pending || self.state.phase == "loading" {
                    out.push(Action::Complete {
                        id,
                        outcome: Err(unavailable("Identity is busy")),
                    });
                    return;
                }
                self.state.error = None;
                if key == "refresh" {
                    if self.epoch.is_some() {
                        self.read("identity.credentials", out);
                        self.read("identity.sessions", out);
                    } else if self.state.phase == "error" {
                        self.http("identity.fetch", None, None, out);
                    }
                    out.push(Action::Complete {
                        id,
                        outcome: Ok(Value::Null),
                    });
                } else if key == self.register
                    || matches!(
                        key.as_str(),
                        "identity.password.acquire" | "identity.release"
                    )
                {
                    self.state.pending = true;
                    self.http(&key, payload, Some(id), out);
                } else {
                    out.push(Action::Complete {
                        id,
                        outcome: Err(unavailable("Unknown command")),
                    });
                }
            }
            Input::Http {
                generation,
                id,
                result,
            } => {
                if generation != self.generation {
                    return;
                }
                let Some(p) = self.pending.remove(&id) else {
                    return;
                };
                // Failure to obtain a validated, correlated outcome is different
                // from a server-declared refusal: the mutation may have committed.
                let (decoded, untrusted) =
                    match result.and_then(|wire| decode_completion(&wire, &id)) {
                        Ok(outcome) => (outcome, false),
                        Err(error) => (Err(error), true),
                    };
                if let Some(command) = p.command {
                    self.state.pending = false;
                    self.state.error = decoded.as_ref().err().cloned();
                    let success = decoded.is_ok();
                    // A lost response has unknown mutation outcome. Refetch identity,
                    // never resend a password or release command.
                    let uncertain =
                        untrusted || matches!(&decoded, Err(Error::UnavailableError { .. }));
                    out.push(Action::Complete {
                        id: command,
                        outcome: decoded,
                    });
                    if success || uncertain || self.session_ended {
                        self.reset(out);
                        self.http("identity.fetch", None, None, out);
                    }
                } else if p.key == "identity.fetch" {
                    match decoded {
                        Ok(value)
                            if value
                                .get("identityId")
                                .is_some_and(|id| id.is_null() || id.is_string()) =>
                        {
                            self.state.error = None;
                            self.state.identity_id = value["identityId"].as_str().map(String::from);
                            self.state.phase = if self.state.identity_id.is_some() {
                                "identified"
                            } else {
                                "anonymous"
                            };
                            if self.state.identity_id.is_some() {
                                self.state.connection = "connecting";
                                out.push(Action::Connect {
                                    generation: self.socket_generation,
                                });
                            }
                        }
                        result => {
                            self.state.phase = "error";
                            self.state.error = Some(
                                result
                                    .err()
                                    .unwrap_or_else(|| invalid("Invalid identity result")),
                            );
                        }
                    }
                }
            }
            Input::BuildMismatch { generation } => {
                if generation == self.generation {
                    self.reset(out);
                    out.push(Action::Reload);
                }
            }
            Input::Frame { generation, wire } => {
                if generation != self.socket_generation {
                    return;
                }
                let Ok(event) = serde_json::from_str::<Value>(&wire) else {
                    self.state.error = Some(invalid("Invalid socket event"));
                    return;
                };
                match event["key"].as_str() {
                    Some("transport.epoch") => {
                        let Some(epoch) = event["payload"]["epoch"].as_str() else {
                            return;
                        };
                        self.epoch = Some(epoch.into());
                        self.message_sequence = 0;
                        self.retry = 250;
                        self.state.connection = "connected";
                        self.state.error = None;
                        self.read("identity.credentials", out);
                        self.read("identity.sessions", out);
                    }
                    Some("transport.complete") => {
                        let Some(id) = event["target"].as_str() else {
                            return;
                        };
                        let Some(p) = self.pending.remove(id) else {
                            return;
                        };
                        match completion(&wire, id) {
                            Ok(value) => {
                                let field = if p.key == "identity.credentials" {
                                    "credentials"
                                } else {
                                    "sessions"
                                };
                                if let Some(values) = value[field].as_array() {
                                    if field == "credentials" {
                                        self.state.credentials = values.clone();
                                    } else {
                                        self.state.sessions = values.clone();
                                    }
                                } else {
                                    self.state.error = Some(invalid("Invalid identity collection"));
                                }
                            }
                            Err(error) => self.state.error = Some(error),
                        }
                    }
                    Some("identity.sessions.changed") => self.read("identity.sessions", out),
                    Some("identity.credentials.changed") => self.read("identity.credentials", out),
                    _ => {}
                }
            }
            Input::Disconnected { generation, code } => {
                if generation != self.socket_generation {
                    return;
                }
                if code == 4003 {
                    self.reset(out);
                    out.push(Action::Reload);
                    return;
                }
                if code == 4001 {
                    // A release can terminate its socket before its HTTP response
                    // arrives. Clear authority immediately, but preserve that
                    // command's result observation until the HTTP carrier settles.
                    if self.state.pending {
                        self.session_ended = true;
                        self.socket_generation += 1;
                        self.epoch = None;
                        self.state.phase = "loading";
                        self.state.connection = "disconnected";
                        self.state.identity_id = None;
                        self.state.credentials.clear();
                        self.state.sessions.clear();
                        self.pending.retain(|_, p| p.command.is_some());
                        out.push(Action::Disconnect);
                        return;
                    }
                    self.reset(out);
                    self.http("identity.fetch", None, None, out);
                    return;
                }
                self.epoch = None;
                self.state.connection = "disconnected";
                self.socket_generation += 1;
                out.push(Action::Disconnect);
                self.pending
                    .retain(|_, p| p.command.is_some() || p.key == "identity.fetch");
                if self.state.identity_id.is_some() {
                    out.push(Action::Retry {
                        generation: self.socket_generation,
                        milliseconds: self.retry,
                    });
                    self.retry = (self.retry * 2).min(5_000);
                }
            }
            Input::Retry { generation } => {
                if generation == self.socket_generation
                    && self.state.identity_id.is_some()
                    && self.state.connection == "disconnected"
                {
                    self.state.connection = "connecting";
                    out.push(Action::Connect { generation });
                }
            }
            Input::ReadTimeout { generation, id } => {
                if generation == self.socket_generation && self.pending.contains_key(&id) {
                    self.state.error = Some(unavailable("Identity read timed out"));
                    self.update(
                        Input::Disconnected {
                            generation,
                            code: 1006,
                        },
                        out,
                    );
                }
            }
            Input::Close => {
                self.reset(out);
                self.closed = true;
                self.state.phase = "closed";
            }
        }
    }
}

pub fn completion(wire: &str, id: &str) -> Outcome {
    decode_completion(wire, id)?
}

fn decode_completion(wire: &str, id: &str) -> Result<Outcome, Error> {
    let event: Value = serde_json::from_str(wire).map_err(|_| invalid("Invalid completion"))?;
    if event["key"] != "transport.complete" || event["target"] != id {
        return Err(invalid("Mismatched completion"));
    }
    match event["payload"]["ok"].as_bool() {
        Some(true) => Ok(Ok(event["payload"]["payload"].clone())),
        Some(false) => Ok(Err(serde_json::from_value(
            event["payload"]["error"].clone(),
        )
        .map_err(|_| invalid("Invalid error"))?)),
        None => Err(invalid("Invalid outcome")),
    }
}
fn invalid(message: &str) -> Error {
    Error::ContractViolationError {
        message: message.into(),
    }
}
fn unavailable(message: &str) -> Error {
    Error::UnavailableError {
        message: message.into(),
    }
}
