use super::State;
use crate::Snapshot;
use alloc::{boxed::Box, vec};
use snap_host::CommitContext;
use snap_store::{Backend, Error, Transaction};
use snap_transport::Value;

/// Blocking host IO after desired-state persistence. Meaningful observations
/// request another pass; failed callbacks persist a blocked lifecycle.
pub type Controller<B> =
    Box<dyn FnMut(&mut ControllerContext<'_, '_, B>, Snapshot) -> Result<(), Error> + Send>;

/// Explicit Document access and the original invocation's progress capability.
/// No runtime borrow, dispatcher, peer replacement or admission capability.
pub struct ControllerContext<'a, 'host, B: Backend> {
    pub(super) state: &'a mut State,
    pub(super) commits: &'a mut CommitContext<'host, B>,
}
impl<B: Backend> ControllerContext<'_, '_, B> {
    pub fn document(&mut self, id: &str) -> Result<Snapshot, Error> {
        self.commits.store.load_keys(
            crate::server::TABLES[0],
            &[vec![snap_store::Value::Text(id.into())]]
                .into_iter()
                .collect(),
        )?;
        let snapshot = self.commits.store.inspect("controller.document", |tx| {
            self.state.document.retained(tx, id)
        })?;
        self.state.pinned.insert(id.into());
        Ok(snapshot)
    }
    pub fn lifecycle(&mut self, id: &str) -> Result<crate::lifecycle::Lifecycle, Error> {
        self.commits.store.inspect("controller.state", |tx| {
            self.state.document.lifecycle(tx, id)
        })
    }
    pub fn finalize(&mut self, id: &str, key: &str) -> Result<(), Error> {
        self.commits.transact("controller.finalize", |tx| {
            let mut lifecycle = self.state.document.lifecycle(tx, id)?;
            lifecycle.finalizers.remove(key);
            self.state.document.set_lifecycle(tx, id, &lifecycle)
        })?;
        self.state.observed(self.commits)
    }
    pub fn inspect<T>(
        &mut self,
        operation: &str,
        read: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.commits.store.inspect(operation, read)
    }
    pub fn transact<T>(
        &mut self,
        operation: &str,
        handler: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let value = self.commits.transact(operation, handler)?;
        // Notify and synchronize immediately, including writes of the controller
        // currently running. No callback must be removed from its registry.
        self.state.observed(self.commits)?;
        Ok(value)
    }
    pub fn progress(&mut self, value: Value) -> Result<(), Error> {
        if let Some(invocation) = &self.commits.invocation {
            invocation.progress.send(value)?;
        }
        Ok(())
    }
}
