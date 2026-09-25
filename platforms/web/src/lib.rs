//! Selected HTTP/WebSocket compatibility bindings, shared by web hosts and clients.
#![no_std]
extern crate alloc;
pub mod cookie;
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_protocol::{ConnectionEvent, Disconnect, Error, Invocation, Outcome, Value, json};

/// Provider result projected by web hosts after the authoritative workflow completes.
pub struct Lease {
    pub id: String,
    pub expires_at: u64,
}
pub struct Reply {
    pub outcome: Outcome,
    pub empty: bool,
    pub lease: Option<Lease>,
    pub cookie: Option<Option<String>>,
    pub terminate: Vec<String>,
}
impl Reply {
    pub fn new(outcome: Outcome) -> Self {
        Self {
            outcome,
            empty: false,
            lease: None,
            cookie: None,
            terminate: Vec::new(),
        }
    }
}
impl snap_protocol::Rejection for Reply {
    fn rejected(error: Error) -> Self {
        Self::new(Err(error))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub key: &'static str,
    pub http: Option<Method>,
    pub socket: bool,
}
impl Binding {
    pub fn get(key: &'static str) -> Self {
        Self {
            key,
            http: Some(Method::Get),
            socket: false,
        }
    }
}
pub fn identity(register: &'static str) -> Vec<Binding> {
    [
        Binding {
            key: register,
            http: Some(Method::Post),
            socket: false,
        },
        Binding::get("identity.fetch"),
        Binding {
            key: "identity.password.acquire",
            http: Some(Method::Post),
            socket: false,
        },
        Binding {
            key: "identity.release",
            http: Some(Method::Post),
            socket: false,
        },
        Binding {
            key: "identity.credentials",
            http: None,
            socket: true,
        },
        Binding {
            key: "identity.sessions",
            http: None,
            socket: true,
        },
    ]
    .into()
}
/// The portable identity controller submits validated account/session commands.
/// This profile binds discovery to GET and independent mutations to POST.
pub fn identity_method(key: &str) -> Method {
    if key == "identity.fetch" {
        Method::Get
    } else {
        Method::Post
    }
}

#[derive(Debug, Serialize)]
pub struct Completion {
    key: &'static str,
    target: String,
    payload: Value,
}
impl Completion {
    pub fn new(target: String, outcome: Outcome) -> Self {
        Self {
            key: "transport.complete",
            target,
            payload: match outcome {
                Ok(payload) => json!({"ok":true,"payload":payload}),
                Err(error) => json!({"ok":false,"error":error}),
            },
        }
    }
    pub fn session_changed(mut self) -> Self {
        self.payload["sessionChanged"] = Value::Bool(true);
        self
    }
    pub fn empty(mut self) -> Self {
        if let Some(payload) = self.payload.as_object_mut() {
            payload.remove("payload");
        }
        self
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    key: String,
    target: Option<String>,
    payload: Option<Value>,
}
pub fn decode_completion(wire: &str, id: &str) -> Result<Outcome, Error> {
    let event: Event =
        serde_json::from_str(wire).map_err(|_| invalid("Invalid Transport completion Event"))?;
    if event.key != "transport.complete" || event.target.as_deref() != Some(id) {
        return Err(invalid("Invalid Transport completion Event"));
    }
    let payload = event
        .payload
        .ok_or_else(|| invalid("Missing Transport result"))?;
    match payload.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(Ok(payload.get("payload").cloned().unwrap_or(Value::Null))),
        Some(false) => Ok(Err(serde_json::from_value(
            payload
                .get("error")
                .cloned()
                .ok_or_else(|| invalid("Missing Transport error"))?,
        )
        .map_err(|_| invalid("Invalid Transport error"))?)),
        None => Err(invalid("Invalid Transport result outcome")),
    }
}
/// One physical connection owns web sequence framing and maps completion targets
/// back to opaque controller IDs. Dropping it discards every unfinished mapping;
/// the controller's generation fencing rejects observations from old attachments.
#[derive(Default)]
pub struct Connection {
    epoch: Option<String>,
    sequence: u64,
    pending: BTreeMap<String, String>,
}
impl Connection {
    pub fn encode(&mut self, mut invocation: Invocation) -> Result<String, Error> {
        let epoch = self
            .epoch
            .as_ref()
            .ok_or_else(|| invalid("Connection is not attached"))?;
        if self.sequence >= 9_007_199_254_740_991 {
            return Err(invalid("Connection sequence exhausted"));
        }
        self.sequence += 1;
        let wire_id = format!("{epoch}:{}", self.sequence);
        let id = core::mem::replace(&mut invocation.operation_id, wire_id.clone());
        let wire = serde_json::to_string(&invocation).map_err(|_| invalid("Invalid invocation"))?;
        self.pending.insert(wire_id, id);
        Ok(wire)
    }
    pub fn event(&mut self, wire: &str) -> Result<ConnectionEvent, Error> {
        let event: Value =
            serde_json::from_str(wire).map_err(|_| invalid("Invalid socket event"))?;
        let key = event["key"]
            .as_str()
            .ok_or_else(|| invalid("Invalid socket event"))?;
        match key {
            "transport.epoch" => {
                if self.epoch.is_some() {
                    return Err(invalid("Connection is already attached"));
                }
                self.epoch = Some(
                    event["payload"]["epoch"]
                        .as_str()
                        .ok_or_else(|| invalid("Invalid epoch"))?
                        .into(),
                );
                Ok(ConnectionEvent::Attached)
            }
            "transport.complete" => {
                let id = event["target"]
                    .as_str()
                    .ok_or_else(|| invalid("Missing target"))?;
                let outcome = decode_completion(wire, id)?;
                let id = self
                    .pending
                    .remove(id)
                    .ok_or_else(|| invalid("Unknown completion target"))?;
                Ok(ConnectionEvent::Completed { id, outcome })
            }
            "transport.ack" => {
                let target = event["target"]
                    .as_str()
                    .ok_or_else(|| invalid("Missing target"))?;
                let id = self
                    .pending
                    .get(target)
                    .ok_or_else(|| invalid("Unknown acceptance target"))?
                    .clone();
                Ok(ConnectionEvent::Accepted { id })
            }
            _ => Ok(ConnectionEvent::Notification { key: key.into() }),
        }
    }
}
pub fn disconnect(code: u16) -> Disconnect {
    match code {
        4001 => Disconnect::AuthorityEnded,
        4003 => Disconnect::BuildChanged,
        _ => Disconnect::Interrupted,
    }
}
pub fn sequence(id: &str) -> Option<(&str, u64)> {
    let (epoch, n) = id.rsplit_once(':')?;
    Some((epoch, n.split('#').next()?.parse().ok()?))
}
fn invalid(message: &str) -> Error {
    Error::ContractViolationError {
        message: message.into(),
    }
}
