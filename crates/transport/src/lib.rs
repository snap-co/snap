//! Portable transport. Authority supplies identities; sessions remain private to it.
//! Applications own entry points. Platforms own IO, clocks and connection handles.
#![no_std]
extern crate alloc;

pub mod client;
pub mod server;

use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};
pub use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Error {
    InvalidInput,
    InvalidOutput,
    UnknownOperation,
    IdentityRequired,
    InvalidBearer,
    Occupied,
    StaleConnection,
    Capacity,
    Protocol,
    Unavailable,
    Application(Value),
}
pub type Outcome = Result<Value, Error>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invocation {
    pub id: u64,
    pub operation: String,
    pub input: Value,
}

/// Commands are carried inside a physical attachment. Clients never supply the
/// server's attachment handle or an asserted identity in the wire envelope.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    Request {
        bearer: Option<String>,
        invocation: Invocation,
    },
    Connect {
        bearer: String,
        client_id: String,
    },
    Invoke(Invocation),
    Disconnect,
    Close,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Accepted { id: u64 },
    Completed { id: u64, outcome: Outcome },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Attached { resumed: bool },
    Detached,
    Events(Vec<Event>),
    Failed(Error),
}

/// Host IO boundary. An exchange delivers one command's ordered observations.
/// IO failure has unknown mutation outcome: callers must never replay mutations
/// automatically. Replacement channels may reconnect using the same client ID.
pub trait Channel {
    fn exchange(
        &mut self,
        command: Command,
    ) -> impl core::future::Future<Output = Result<Response, Error>>;
}
