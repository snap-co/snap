use crate::blocking::Peer;
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use snap_store::{Backend, Data, Error, RowChange, Store, Transaction};
use snap_transport::{
    Event, Response, Value, bearer::Authority, operation::Validator, runtime::Output,
};

/// Logical residency survives physical observer loss until Transport retires it.
pub struct Connection {
    pub actor: String,
    pub lifetime: String,
    pub open: bool,
}

/// An original invocation's observation capability. It cannot submit work or
/// publish under another ID, and a reconnect cannot replace its physical output.
pub struct Progress {
    output: Output,
    id: u64,
    validate: Validator,
}
impl Progress {
    pub(crate) fn new(output: Output, id: u64, validate: Validator) -> Self {
        Self {
            output,
            id,
            validate,
        }
    }
    pub fn send(&self, value: Value) -> Result<(), Error> {
        if !(self.validate)(&value) {
            return Err(Error::Invalid);
        }
        self.output
            .push_back(Response::Event(Event::Progress { id: self.id, value }));
        Ok(())
    }
    /// A module may retire an optimistic write before controller observations,
    /// without ending the invocation. Terminal completion belongs to the engine.
    pub fn publish(&self, kind: &str, input: Value) {
        self.output.push_back(Response::Global {
            kind: kind.into(),
            input,
        });
    }
}
pub struct InvocationScope<'a> {
    pub connection: Option<u64>,
    pub progress: &'a Progress,
}

/// Commit/controller access, without the executor or dispatch registry. Store
/// commits are journaled for the engine to notify before another controller pass.
/// A module needing immediate synchronization can consume the journal itself.
pub struct CommitContext<'a, B: Backend> {
    pub store: &'a mut Store<B>,
    pub connections: &'a BTreeMap<u64, Connection>,
    pub peers: &'a BTreeMap<u64, Peer>,
    pub authority: &'a dyn Authority,
    pub data: &'a Data,
    pub invocation: Option<InvocationScope<'a>>,
    changes: Vec<RowChange>,
}
impl<'a, B: Backend> CommitContext<'a, B> {
    pub(crate) fn new(
        store: &'a mut Store<B>,
        connections: &'a BTreeMap<u64, Connection>,
        peers: &'a BTreeMap<u64, Peer>,
        authority: &'a dyn Authority,
        data: &'a Data,
        invocation: Option<InvocationScope<'a>>,
    ) -> Self {
        Self {
            store,
            connections,
            peers,
            authority,
            data,
            invocation,
            changes: Vec::new(),
        }
    }
    pub fn transact<T>(
        &mut self,
        operation: &str,
        handler: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let committed = self.store.run(operation, handler)?;
        self.changes.extend(committed.changes);
        Ok(committed.value)
    }
    pub fn take_changes(&mut self) -> Vec<RowChange> {
        core::mem::take(&mut self.changes)
    }
    pub(crate) fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }
}

/// Module-selected commit interpretation and synchronous reconciliation. The
/// engine calls `committed` only after successful persistence, then drives one
/// `reconcile` pass at a time until idle, including controller-generated commits.
/// An error must consume the failed pass; it must not endlessly requeue itself.
/// Controllers may block on host IO, but cannot reenter dispatch. A controller
/// failure cannot undo a commit. Recovery and retry policy belong to the module.
pub trait Participant<B: Backend> {
    /// Prepare bootstrap/internal transaction data. Dispatch uses the selected
    /// operation's data declaration instead, before admission.
    fn prepare(&mut self, _: &mut CommitContext<'_, B>) -> Result<(), Error> {
        Ok(())
    }
    /// Update residency and retired-lifetime housekeeping only. This must not
    /// execute application controllers or release accepted work's data.
    fn maintain(&mut self, _: &mut CommitContext<'_, B>) -> Result<(), Error> {
        Ok(())
    }
    /// Pin module-specific requirements of the accepted logical connection.
    fn accepted(&mut self, _: Option<u64>) {}
    /// Interpret successful commit changes and module-owned publication data.
    /// This also runs for read-only operations that declare publications.
    fn committed(
        &mut self,
        _: &mut CommitContext<'_, B>,
        _: &[RowChange],
        _: &Value,
    ) -> Result<(), Error> {
        Ok(())
    }
    /// Run one pending controller pass. Return false only when idle. Consume
    /// failed passes before returning an error so independent work can drain.
    fn reconcile(&mut self, _: &mut CommitContext<'_, B>) -> Result<bool, Error> {
        Ok(false)
    }
    /// Rebuild pending notifications from persisted state after a host restart.
    fn recover(&mut self, _: &mut CommitContext<'_, B>) -> Result<(), Error> {
        Ok(())
    }
    /// Release temporary pins after all passes. The operation's full-table data
    /// requirement has ended; finalizers and logical residency remain owned.
    fn release(&mut self, _: &mut CommitContext<'_, B>) -> Result<(), Error> {
        Ok(())
    }
    /// Filter direct-host delivery. Invocation Events retain accepted authority;
    /// only ongoing module publications use current read authorization.
    fn filter(
        &mut self,
        _: &mut CommitContext<'_, B>,
        _: u64,
        _: &mut Response,
    ) -> Result<bool, Error> {
        Ok(true)
    }
}
impl<B: Backend> Participant<B> for () {}
