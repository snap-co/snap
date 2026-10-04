//! Bearers are opaque to Transport. Providers own validation and persistence;
//! receivers stage replacement credentials for delivery after durable commit.
use crate::{Error, Outcome};
use alloc::{string::String, sync::Arc};
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

/// Captured authentication facts. A callback-only host can supply an actor
/// without claiming Identity's session freshness facts.
#[derive(Default)]
pub struct Resolved {
    pub actor: Option<String>,
    pub principal: Option<Principal>,
}
/// Host-supplied credential resolution over resident state. Implementations own
/// credential policy; consumers capture these facts before accepting work and
/// must not resolve credentials again to execute an accepted operation.
pub trait Authority: Send + Sync {
    fn identify(&self, tx: &mut Transaction<'_>, bearer: &str)
    -> Result<String, snap_store::Error>;
    /// Retained login eligibility is not current access authority.
    fn retained(
        &self,
        tx: &mut Transaction<'_>,
        bearer: &str,
    ) -> Result<String, snap_store::Error> {
        self.identify(tx, bearer)
    }
    /// Resolve admission facts. `identity_required` is true for connected work
    /// even when its operation permits anonymous requests. Providers must not
    /// downgrade invalid credentials to anonymous in that case. Operation
    /// contracts separately reject a missing identity when one is required.
    fn resolve(
        &self,
        tx: &mut Transaction<'_>,
        bearer: Option<&str>,
        _identity_required: bool,
    ) -> Result<Resolved, snap_store::Error> {
        Ok(Resolved {
            actor: bearer.map(|bearer| self.identify(tx, bearer)).transpose()?,
            principal: None,
        })
    }
}

/// Resident-only identity lookup supplied by application assembly. Hosts arrange
/// its data before use; external IO cannot run inside a Store transaction.
pub type Identify =
    Arc<dyn Fn(&mut Transaction<'_>, &str) -> Result<String, snap_store::Error> + Send + Sync>;

/// Adapt host callbacks to the credential interface without owning their policy.
pub struct Callbacks {
    identify: Identify,
    retained: Identify,
}
impl Callbacks {
    pub fn new(identify: Identify) -> Self {
        Self::with_retained(identify.clone(), identify)
    }
    pub fn with_retained(identify: Identify, retained: Identify) -> Self {
        Self { identify, retained }
    }
}
impl Authority for Callbacks {
    fn identify(
        &self,
        tx: &mut Transaction<'_>,
        bearer: &str,
    ) -> Result<String, snap_store::Error> {
        (self.identify)(tx, bearer)
    }
    fn retained(
        &self,
        tx: &mut Transaction<'_>,
        bearer: &str,
    ) -> Result<String, snap_store::Error> {
        (self.retained)(tx, bearer)
    }
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
