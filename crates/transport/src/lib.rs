//! Portable transport. Authority supplies identities; sessions remain private to it.
//! Applications own entry points. Platforms own IO, clocks and connection handles.
#![cfg_attr(
    not(all(
        not(target_family = "wasm"),
        any(feature = "native-server", feature = "native-client")
    )),
    no_std
)]
extern crate alloc;
extern crate self as snap_transport;

pub mod bearer;
pub mod binary;
pub mod carrier;
pub mod client;
pub mod execution;
pub mod host;
pub mod inbox;
pub mod lane;
#[cfg(all(
    not(target_family = "wasm"),
    any(feature = "native-server", feature = "native-client")
))]
pub mod native;
pub mod operation;
pub mod runtime;
pub mod server;
pub mod subscription;

use alloc::string::String;
use serde::{Deserialize, Serialize};
pub use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Error {
    InvalidInput,
    InvalidOutput,
    InvalidState,
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

/// A subscription key. Transport routes `Global` payloads on this alone and never
/// inspects them; `kind` namespaces a capability's events and may be empty when an
/// identifier is already globally unique, as document IDs are.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Topic {
    pub kind: String,
    pub id: String,
}

impl Topic {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

/// SDK operation contract. Progress is independent of terminal output/error and
/// may be a shared message type across operations.
pub trait Operation {
    const NAME: &'static str;
    type Input: Serialize;
    type Output: serde::de::DeserializeOwned;
    type Error: serde::de::DeserializeOwned;
    type Progress: serde::de::DeserializeOwned;
}

#[derive(Debug)]
pub enum Failure<E> {
    Transport(Error),
    Application(E),
}

/// `id` correlates this submission's frames. It is not an idempotency key:
/// duplicate detection and recovery belong to operation guards or handlers.
/// Clients must distinguish their outstanding calls to correlate replies; the
/// server does not keep an invocation-ID history to enforce that for them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Invocation {
    pub id: u64,
    pub operation: String,
    pub input: Value,
}

/// Commands travel over a physical channel. Clients never supply the
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

/// One server-to-client observation. Every variant carries the invocation it
/// belongs to, and only that invocation's originator ever receives it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// Admission. Not acceptance of the operation's effect.
    Accepted { id: u64 },
    /// Carrier credential publication, distinct from module output. Sent only
    /// after commit, never retained in invocation diagnostics or replay records.
    Bearer { id: u64, change: bearer::Change },
    /// Transient work update. Originates after durable commit, from platform
    /// controllers rather than operation handlers.
    Progress { id: u64, value: Value },
    /// Terminal. Transport consumes this and returns the invocation's output.
    Completed { id: u64, outcome: Outcome },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Attached {
        resumed: bool,
    },
    Detached,
    /// Exactly one observation. Batching, if a platform wants it, is negotiated
    /// below this layer and must be reassembled into single events on arrival.
    Event(Event),
    /// The invocation never began: refused admission, unknown operation, or a
    /// carrier that could not deliver it. Distinct from `Event::Completed` with
    /// a failed outcome, which means the operation ran and its effect is known.
    Failed(Error),
    /// Uncorrelated server push, routed by subscription rather than by
    /// invocation. Reaches every logical connection subscribed to `kind`, not
    /// only an originator. The payload is opaque to Transport.
    Global {
        kind: String,
        input: Value,
    },
}

/// Host IO boundary: a bidirectional event stream over one already-established
/// channel, not a request/response exchange.
///
/// [`Self::send`] and [`Self::receive`] are independent. A command returns as
/// soon as it is queued, and its observations arrive later as separate frames,
/// correlated to the invocation that produced them. Acceptance must therefore be
/// publishable before the handler runs, or a slow operation — several seconds of
/// model round trips, minutes of tool calls — would sit in dead air with the
/// client unable to distinguish "working" from "hung".
///
/// `receive` yields `None` once the channel is closed. IO failure has unknown
/// mutation outcome: callers must never replay mutations automatically.
/// Replacement channels may reconnect using the same client ID, and stable
/// invocation ids correlate frames, but do not promise cached results or safe
/// resubmission. Operations define their own recovery behavior.
pub trait Channel {
    fn send(&mut self, command: Command) -> impl core::future::Future<Output = Result<(), Error>>;
    fn receive(&mut self) -> impl core::future::Future<Output = Result<Option<Response>, Error>>;
}
