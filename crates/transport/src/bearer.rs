//! Bearers are opaque to Transport. Providers own validation and persistence;
//! receivers stage replacement credentials for delivery after durable commit.
use crate::{Error, Outcome};
use alloc::string::String;
use snap_store::{Data, Transaction};

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Token(String);
impl Token {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl core::fmt::Debug for Token {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Token([redacted])")
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Change {
    Set(Token),
    Clear,
}

/// Public authentication facts, never a persisted session or its storage key.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub identity: String,
    pub authenticated_at: i64,
}
pub trait Provider: Send + Sync {
    fn data(&self) -> Data;
    fn identify(
        &self,
        tx: &mut Transaction<'_>,
        bearer: &str,
        now: i64,
    ) -> Result<Principal, snap_store::Error>;
}
pub trait Receiver {
    fn bearer_changed(&mut self, change: Change) -> Result<(), Error>;
}

/// Private host-to-carrier handoff. The bearer is separate from operation output.
#[derive(Debug)]
pub struct Reply {
    /// Actual admission, including operations that fail after acceptance.
    pub accepted: bool,
    pub outcome: Outcome,
    pub bearer: Option<Change>,
}
impl From<Outcome> for Reply {
    fn from(outcome: Outcome) -> Self {
        Self {
            accepted: false,
            outcome,
            bearer: None,
        }
    }
}
