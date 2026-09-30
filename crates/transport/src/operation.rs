//! Store-backed operation declarations, composed before accepting traffic.
//! Guards inspect resident state under the dispatch gate. Handlers stage writes
//! in the caller's transaction; the platform owns loading and durable commit.
use crate::execution::{Admission, Attempt, Call, Executor, Program, Ticket, View, WorkingSet};
use crate::{Error, Invocation, Value};
use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use snap_store::{Backend, RowChange, Store, Transaction};

pub type Validator = fn(&Value) -> bool;
/// Universal operation schema. JSON programs and transactional definitions use
/// this contract; the state adapter does not define another protocol or schema.
pub struct Contract<N = String> {
    pub key: N,
    pub identity_required: bool,
    pub input: Validator,
    pub output: Validator,
    pub error: Validator,
}
impl<N> Contract<N> {
    pub fn validate(&self, input: &Value, actor: Option<&str>) -> Result<(), Error> {
        if !(self.input)(input) {
            return Err(Error::InvalidInput);
        }
        if self.identity_required && actor.is_none_or(str::is_empty) {
            return Err(Error::IdentityRequired);
        }
        Ok(())
    }
    pub fn checked_error(&self, error: Error) -> Error {
        if let Error::Application(value) = &error
            && !(self.error)(value)
        {
            Error::InvalidOutput
        } else {
            error
        }
    }
}
/// Identity and private credential are supplied by trusted platform composition.
/// A credential may support admission policy or attribution, never reauthorization
/// of accepted work in a nested persistence helper.
#[derive(Default)]
pub struct Context {
    pub actor: Option<String>,
    pub bearer: Option<String>,
    /// Trusted logical lifetime, never copied from invocation input.
    pub lifetime: Option<String>,
    /// Module-owned acceptance data. Guards may capture resident data here;
    /// handlers consume it without reevaluating policy.
    pub prepared: Value,
    /// Declarative publication data, exposed only after durable commit.
    pub publication: Value,
    /// Read-only platform inputs declared by the selected definition. Application
    /// code reads these values, never the physical clock, entropy source or IO.
    pub inputs: BTreeMap<String, Value>,
}

pub enum Failure {
    Store(snap_store::Error),
    Rejected(Error),
}
impl From<snap_store::Error> for Failure {
    fn from(error: snap_store::Error) -> Self {
        Self::Store(error)
    }
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Rejected(error)
    }
}

type Check = dyn Fn(&mut Transaction<'_>, &Invocation, &mut Context) -> Result<(), Failure> + Send;
type Policy =
    fn(&mut Transaction<'_>, Option<&str>, &Value, Option<&str>) -> Result<(), snap_store::Error>;
pub struct Guard(Box<Check>);
impl Guard {
    pub fn new(
        check: impl Fn(&mut Transaction<'_>, &Invocation, &mut Context) -> Result<(), Failure>
        + Send
        + 'static,
    ) -> Self {
        Self(Box::new(check))
    }
    pub fn policy(check: Policy) -> Self {
        Self::new(move |tx, call, context| {
            check(
                tx,
                context.actor.as_deref(),
                &call.input,
                context.bearer.as_deref(),
            )
            .map_err(Into::into)
        })
    }
}
type Callback = dyn FnMut(
        &mut Transaction<'_>,
        &Invocation,
        Option<&str>,
        Option<&str>,
        &mut Context,
    ) -> Result<Value, Failure>
    + Send;
pub struct Handler(Box<Callback>);
impl Handler {
    pub fn new(
        mut run: impl FnMut(
            &mut Transaction<'_>,
            &Invocation,
            Option<&str>,
            Option<&str>,
            &mut Context,
        ) -> Result<Value, snap_store::Error>
        + Send
        + 'static,
    ) -> Self {
        Self(Box::new(move |tx, call, actor, bearer, context| {
            run(tx, call, actor, bearer, context).map_err(Into::into)
        }))
    }
}
pub enum TypedFailure<E> {
    Store(snap_store::Error),
    Application(E),
}
impl<E> From<snap_store::Error> for TypedFailure<E> {
    fn from(error: snap_store::Error) -> Self {
        Self::Store(error)
    }
}

pub struct Definition {
    pub name: String,
    pub identity_required: bool,
    pub input: Validator,
    pub output: Validator,
    pub error: Validator,
    pub progress: Validator,
    /// Evaluated once in declaration order. First failure stops acceptance.
    pub guards: Vec<Guard>,
    /// Explicit Store residency, loaded by the platform before any guard runs.
    pub tables: &'static [&'static str],
    pub inputs: &'static [&'static str],
    pub handler: Handler,
}

impl Definition {
    /// Server declaration from the same typed contract the SDK consumes. Type
    /// erasure happens only at universal JSON ingress/egress, not in feature code.
    pub fn typed<O>(
        identity_required: bool,
        guards: Vec<Guard>,
        tables: &'static [&'static str],
        inputs: &'static [&'static str],
        mut run: impl FnMut(
            &mut Transaction<'_>,
            O::Input,
            &mut Context,
        ) -> Result<O::Output, TypedFailure<O::Error>>
        + Send
        + 'static,
    ) -> Self
    where
        O: crate::Operation,
        O::Input: serde::de::DeserializeOwned,
        O::Output: serde::Serialize,
        O::Error: serde::Serialize,
    {
        Self {
            name: O::NAME.into(),
            identity_required,
            guards,
            tables,
            inputs,
            input: |value| serde_json::from_value::<O::Input>(value.clone()).is_ok(),
            output: |value| serde_json::from_value::<O::Output>(value.clone()).is_ok(),
            error: |value| serde_json::from_value::<O::Error>(value.clone()).is_ok(),
            progress: |value| serde_json::from_value::<O::Progress>(value.clone()).is_ok(),
            handler: Handler(Box::new(move |tx, call, _, _, context| {
                let input = serde_json::from_value(call.input.clone())
                    .map_err(|_| Failure::Rejected(Error::InvalidInput))?;
                match run(tx, input, context) {
                    Ok(output) => serde_json::to_value(output)
                        .map_err(|_| Failure::Rejected(Error::InvalidOutput)),
                    Err(TypedFailure::Store(error)) => Err(Failure::Store(error)),
                    Err(TypedFailure::Application(error)) => {
                        Err(Failure::Rejected(Error::Application(
                            serde_json::to_value(error)
                                .map_err(|_| Failure::Rejected(Error::InvalidOutput))?,
                        )))
                    }
                }
            })),
        }
    }
    pub fn contract(&self) -> Contract<&str> {
        Contract {
            key: &self.name,
            identity_required: self.identity_required,
            input: self.input,
            output: self.output,
            error: self.error,
        }
    }
    pub fn admit(
        &self,
        tx: &mut Transaction<'_>,
        invocation: &Invocation,
        context: &mut Context,
    ) -> Result<(), Failure> {
        self.contract()
            .validate(&invocation.input, context.actor.as_deref())?;
        for guard in &self.guards {
            (guard.0)(tx, invocation, context)?;
        }
        Ok(())
    }
}

/// A stable selection, retained with queued work. Resolving a name is separate
/// from evaluating guards, and execution uses this same definition thereafter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Selection(usize);

#[derive(Default)]
pub struct Registry {
    names: BTreeMap<String, Selection>,
    definitions: Vec<Definition>,
}

impl Registry {
    pub fn register(&mut self, definition: Definition) -> Result<Selection, Error> {
        if !valid_name(&definition.name) || self.names.contains_key(&definition.name) {
            return Err(Error::Protocol);
        }
        let selection = Selection(self.definitions.len());
        self.names.insert(definition.name.clone(), selection);
        self.definitions.push(definition);
        Ok(selection)
    }
    pub fn resolve(&self, name: &str) -> Result<Selection, Error> {
        self.names.get(name).copied().ok_or(Error::UnknownOperation)
    }
    pub fn get(&self, selection: Selection) -> &Definition {
        &self.definitions[selection.0]
    }
    /// Execution may mutate handler captures, not the selected contract or guards.
    fn handler(&mut self, selection: Selection) -> &mut Handler {
        &mut self.definitions[selection.0].handler
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    name.len() <= 128
        && name.contains('.')
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}

/// One Store-backed application dispatcher. Physical carriers hold only handles
/// and output buffers. Admission and execution share this lane, selected contract,
/// captured context and resident requirements. The platform drives backend IO.
pub struct Runtime<W> {
    definitions: Registry,
    engine: Executor<Transactional>,
    queued: BTreeMap<Ticket, (W, Invocation, Selection)>,
    slot: Option<Ticket>,
    active: Option<(W, Invocation, Selection, Context)>,
    tables: &'static [&'static str],
    started: bool,
    preparing: bool,
}
impl<W> Default for Runtime<W> {
    fn default() -> Self {
        Self {
            definitions: Registry::default(),
            engine: Executor::new(Transactional, 1024).expect("transactional adapter"),
            queued: BTreeMap::new(),
            slot: None,
            active: None,
            tables: &[],
            started: false,
            preparing: false,
        }
    }
}
/// Store operations use the executor's transactional slots rather than its JSON
/// WorkingSet adapter. There is only one scheduler implementation and gate.
struct Transactional;
impl Program for Transactional {
    fn state_version(&self) -> u64 {
        0
    }
    fn valid_state(&self, state: &Value) -> bool {
        state.is_null()
    }
    fn operations(&self) -> &[crate::execution::Operation] {
        &[]
    }
    fn admit(&self, _: &Call, _: View<'_>) -> Admission {
        Admission::Reject(Error::UnknownOperation)
    }
    fn attempt(&self, _: &Call, _: WorkingSet<'_>) -> Attempt {
        Attempt::Fail(Error::UnknownOperation)
    }
}
pub struct Completed<W> {
    pub work: W,
    pub context: Context,
    pub outcome: crate::Outcome,
    pub changes: Vec<RowChange>,
    pub storage_failure: Option<snap_store::Error>,
}
impl<W> Runtime<W> {
    /// Assembly closes on first submission. Traffic cannot replace handlers or
    /// append names to the operation contract it is already executing.
    pub fn register(&mut self, definition: Definition) -> Result<Selection, Error> {
        if self.started {
            return Err(Error::Protocol);
        }
        self.definitions.register(definition)
    }
    pub fn definitions(&self) -> &Registry {
        &self.definitions
    }
    pub fn idle(&self) -> bool {
        self.engine.idle()
    }
    pub fn pending(&self) -> usize {
        self.queued.len()
    }
    pub fn tables(&self) -> &'static [&'static str] {
        self.tables
    }
    pub fn release_tables(&mut self) {
        self.tables = &[];
    }
    pub fn enqueue(
        &mut self,
        work: W,
        invocation: Invocation,
        selection: Selection,
    ) -> Result<(), Error> {
        if invocation.id == 0 || !(self.definitions.get(selection).input)(&invocation.input) {
            return Err(Error::InvalidInput);
        }
        let ticket = self.engine.reserve()?;
        self.started = true;
        self.queued.insert(ticket, (work, invocation, selection));
        Ok(())
    }
    /// Acquire before platform loading and authentication. No accepted or queued
    /// writer may run while the platform prepares the current slot.
    pub fn acquire(&mut self) -> Option<(W, Invocation, Selection)> {
        let crate::execution::Event::Reserved(ticket) = self.engine.step()? else {
            unreachable!("transactional adapter emits slots only")
        };
        self.slot = Some(ticket);
        self.preparing = true;
        self.queued.remove(&ticket)
    }
    pub fn reject(&mut self) {
        assert!(
            self.active.is_none(),
            "accepted work must drain before releasing its gate"
        );
        self.preparing = false;
        self.engine
            .finish_reserved(self.slot.take().expect("owned slot"))
            .expect("owned transactional slot");
    }
    pub fn accept<B: Backend>(
        &mut self,
        store: &mut Store<B>,
        work: W,
        invocation: Invocation,
        selection: Selection,
        mut context: Context,
    ) -> Result<(), (W, Error)> {
        if !self.preparing {
            return Err((work, Error::Protocol));
        }
        self.preparing = false;
        let mut denied = None;
        let result = store.inspect("transport.admit", |tx| {
            match self
                .definitions
                .get(selection)
                .admit(tx, &invocation, &mut context)
            {
                Ok(()) => Ok(()),
                Err(Failure::Store(error)) => Err(error),
                Err(Failure::Rejected(error)) => {
                    tx.status()?;
                    denied = Some(error);
                    Err(snap_store::Error::Invalid)
                }
            }
        });
        if let Err(error) = result {
            return Err((
                work,
                denied
                    .map(|error| {
                        self.definitions
                            .get(selection)
                            .contract()
                            .checked_error(error)
                    })
                    .unwrap_or_else(|| storage_error(error)),
            ));
        }
        self.tables = self.definitions.get(selection).tables;
        self.active = Some((work, invocation, selection, context));
        Ok(())
    }
    /// Validate output before Store commits. Neither handlers nor commit rerun a
    /// guard. Sticky Store failures take precedence over caught callback failures.
    pub fn execute<B: Backend>(&mut self, store: &mut Store<B>) -> Option<Completed<W>> {
        let (work, invocation, selection, mut context) = self.active.take()?;
        let definition = self.definitions.get(selection);
        let output = definition.output;
        let error_contract = definition.error;
        let mut invalid = false;
        let mut rejected = None;
        let actor = context.actor.clone();
        let bearer = context.bearer.clone();
        let handler = self.definitions.handler(selection);
        let result = store.run("transport.execute", |tx| {
            let value = match (handler.0)(
                tx,
                &invocation,
                actor.as_deref(),
                bearer.as_deref(),
                &mut context,
            ) {
                Ok(value) => value,
                Err(Failure::Store(error)) => return Err(error),
                Err(Failure::Rejected(error)) => {
                    tx.status()?;
                    rejected = Some(
                        if let Error::Application(value) = &error
                            && !error_contract(value)
                        {
                            Error::InvalidOutput
                        } else {
                            error
                        },
                    );
                    return Err(snap_store::Error::Invalid);
                }
            };
            tx.status()?;
            if !output(&value) {
                invalid = true;
                return Err(snap_store::Error::Invalid);
            }
            Ok(value)
        });
        let (outcome, changes, storage_failure) = match result {
            Ok(committed) => (Ok(committed.value), committed.changes, None),
            Err(error) => {
                let storage = (!invalid && rejected.is_none()).then_some(error.clone());
                (
                    Err(rejected.unwrap_or_else(|| {
                        if invalid {
                            Error::InvalidOutput
                        } else {
                            storage_error(error)
                        }
                    })),
                    Vec::new(),
                    storage,
                )
            }
        };
        if outcome.is_err() {
            context.publication = Value::Null;
        }
        Some(Completed {
            work,
            context,
            outcome,
            changes,
            storage_failure,
        })
    }
    /// Platform publication/controller work still owns the lane after commit.
    /// Finish only after its terminal result and logical-resource cleanup.
    pub fn finish(&mut self) {
        self.tables = &[];
        self.reject();
    }
}

pub fn storage_error(error: snap_store::Error) -> Error {
    match error {
        snap_store::Error::Miss(_) => Error::Application(crate::json!({"code":"StoreMiss"})),
        snap_store::Error::NotFound => Error::InvalidBearer,
        snap_store::Error::Invalid | snap_store::Error::Constraint => {
            Error::Application(crate::json!({"code":"Rejected"}))
        }
        snap_store::Error::Unavailable => Error::Unavailable,
        snap_store::Error::Indeterminate => Error::Unavailable,
    }
}
