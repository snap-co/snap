use super::Value;
use alloc::{collections::BTreeMap, string::String};

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    UnknownOperation,
    IdentityRequired,
    InvalidInput,
    InvalidOutput,
    InvalidState,
    Unavailable,
    Protocol,
    Capacity,
    Application(Value),
}
pub type Outcome = Result<Value, Error>;
pub type Validator = fn(&Value) -> bool;

/// Descriptions belong to the selected program. The executor never caches these
/// function pointers across program replacement.
pub struct Operation {
    pub key: &'static str,
    pub identity_required: bool,
    pub input: Validator,
    pub output: Validator,
    pub error: Validator,
}

/// Constructed by trusted host composition, never deserialized from a caller.
#[derive(Clone, Debug)]
pub struct Call {
    pub operation: String,
    pub input: Value,
    pub identity: Option<String>,
}

pub enum Admission {
    Ready,
    Need(String),
    Reject(Error),
}
pub enum Attempt {
    Need(String),
    Commit { state: Value, result: Value },
    Fail(Error),
}
pub enum Stop {
    Need(String),
    Fail(Error),
}
impl From<Error> for Stop {
    fn from(error: Error) -> Self {
        Self::Fail(error)
    }
}

/// Local application helper. `read` consults supplied values only; it makes no
/// host call. A missing key requests a read-only dependency, never an external
/// side effect. The host authorizes/resolves that request in the call's context.
pub struct Inputs<'a> {
    pub(crate) values: &'a BTreeMap<String, Value>,
}
impl Inputs<'_> {
    pub fn read(&self, key: &str) -> Result<&Value, Stop> {
        self.values.get(key).ok_or_else(|| Stop::Need(key.into()))
    }
}
pub struct View<'a> {
    pub state: &'a Value,
    pub connected: bool,
    pub inputs: Inputs<'a>,
}

/// A deep copy of committed JSON data. There are no shared interior-mutable
/// pointers into live state. Drop on Need/Fail rolls back *all* tentative edits.
/// Later reads of `state` see this attempt's earlier writes.
pub struct WorkingSet<'a> {
    pub state: Value,
    pub connected: bool,
    pub inputs: Inputs<'a>,
}
impl WorkingSet<'_> {
    pub fn finish(self, result: Result<Value, Stop>) -> Attempt {
        match result {
            Ok(result) => Attempt::Commit {
                state: self.state,
                result,
            },
            Err(Stop::Need(key)) => Attempt::Need(key),
            Err(Stop::Fail(error)) => Attempt::Fail(error),
        }
    }
}

/// Application entry points. Implementations must keep no invocation state in
/// themselves, perform no IO, and treat supplied inputs as immutable. These are
/// ordinary Rust calls in this build, not a stable binary ABI or an IO sandbox.
/// A future module adapter must transfer owned data before resetting guest scratch.
pub trait Program {
    fn state_version(&self) -> u64;
    fn valid_state(&self, state: &Value) -> bool;
    fn operations(&self) -> &[Operation];
    fn admit(&self, call: &Call, view: View<'_>) -> Admission;
    fn attempt(&self, call: &Call, work: WorkingSet<'_>) -> Attempt;
}
