//! Declarative Store-backed publication bindings. Modules supply extent and wire
//! semantics; the execution host owns peers, logical references and physical IO.
use crate::Value;
use alloc::{boxed::Box, collections::BTreeSet, string::String, vec::Vec};
use snap_store::{Data, Error, Transaction, Value as KeyPart};

pub type Extent = BTreeSet<Vec<KeyPart>>;
type Select = dyn Fn(&mut Transaction<'_>, &str) -> Result<Extent, Error> + Send;
type Read = dyn Fn(&mut Transaction<'_>, &str, &str) -> Result<Value, Error> + Send;
type Changes = dyn Fn(&Value, &Value, &Value, bool) -> Result<Vec<Value>, Error> + Send;
type Origin = dyn Fn(&Value) -> Result<Option<Value>, Error> + Send;
type Filter = dyn Fn(&mut Value, &Extent) -> Result<bool, Error> + Send;
type Expire = dyn Fn(&mut Transaction<'_>, &str) -> Result<(), Error> + Send;

/// A module's topic and replication rules, not an execution participant. State
/// values are module-owned JSON. Publications are visible only after Store commit.
pub struct Definition {
    pub topic: String,
    pub table: &'static str,
    pub data: Data,
    pub extent: Box<Select>,
    pub read: Box<Read>,
    /// Encode differences from the previous physical observer's state. `origin`
    /// identifies the accepted logical connection, never a reconnecting observer.
    pub changes: Box<Changes>,
    /// Optional early optimistic-write retirement, on the original output only.
    pub origin: Box<Origin>,
    /// Reauthorize queued topic data. Invocation Events are never filtered here.
    pub filter: Box<Filter>,
    pub expire: Box<Expire>,
    pub reset: Value,
}
