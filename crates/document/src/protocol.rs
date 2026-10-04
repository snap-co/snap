use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Document visibility mutations. External cleanup, blocking and retry policy are
/// Store-resource and application concerns, not Document mutation behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Delete,
    Archive,
}
impl Visibility {
    pub fn name(self) -> &'static str {
        match self {
            Self::Delete => "document.delete",
            Self::Archive => "document.archive",
        }
    }
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "document.delete" => Some(Self::Delete),
            "document.archive" => Some(Self::Archive),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Error {
    Denied,
    NotFound,
    Invalid,
    Incompatible,
    Rejected(String),
    Diverged(String),
    Protocol,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub kind: String,
    /// Identifies compatible deterministic mutation behavior as well as schema.
    pub version: String,
    pub revision: u64,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    /// Stable within one logical connection; distinct from transport invocation IDs.
    pub id: u64,
    pub document: String,
    pub version: String,
    pub mutation: String,
    pub args: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    pub id: u64,
    pub document: String,
    /// None means the write committed but current authority forbids its contents.
    pub result: Result<Option<Snapshot>, Error>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replication {
    pub intent: Intent,
    /// Verified by the server, never taken from a client's asserted identity.
    pub actor: String,
    pub base_revision: u64,
    pub base_digest: String,
    pub revision: u64,
    pub result_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    pub document: String,
    pub version: String,
    pub revision: u64,
    pub digest: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub holdings: Vec<Holding>,
    pub pending: Vec<Intent>,
}

/// All permitted holdings: changed snapshots plus unchanged validated holdings.
/// Any local document absent from both sets must be removed. No subset policy.
/// Recovered receipts remove journal entries without replacing these newer snapshots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconciliation {
    #[serde(default)]
    pub unchanged: Vec<Holding>,
    pub documents: Vec<Snapshot>,
    pub completed: Vec<Completion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    Manifest(Manifest),
    Mutate(Intent),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    Accepted {
        id: u64,
    },
    Completed(Completion),
    /// Mutation is durably committed before terminal invocation completion.
    /// Reconcile the optimistic journal without closing the invocation channel.
    Committed(Completion),
    Manifest(Reconciliation),
    /// Unsolicited Access-driven replacement; cannot satisfy a recovery request.
    Holdings(Vec<Snapshot>),
    Replication(Replication),
    /// Revocation removes desired holdings and local pending work for these IDs.
    Removed(Vec<String>),
    Reset,
}
