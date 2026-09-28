use crate::{Host, Work};
use snap_document::Snapshot;
use snap_store::{Backend, Error, Transaction};
use snap_transport::{Event, Value};

/// Host-owned external reconciliation. The callback runs synchronously under the
/// application gate, after desired state commits. Persist blocked status before
/// returning an error; subsequent calls must inspect it and no-op until cleared.
pub type Controller<B> =
    Box<dyn FnMut(&mut ControllerContext<'_, B>, Snapshot) -> Result<(), Error> + Send>;

/// Restricted controller access: transactions and progress, without dispatch
/// re-entry. Controllers may do host IO between these calls.
pub struct ControllerContext<'a, B: Backend> {
    pub(super) host: &'a mut Host<B>,
    pub(super) work: Option<&'a Work>,
    pub(super) kind: &'a str,
}

impl<B: Backend> ControllerContext<'_, B> {
    /// Explicitly load a related Document for synchronous reconciliation. It stays
    /// pinned through this invocation's controllers, then normal residency resumes.
    /// Unlike `inspect`, this is host IO and may read storage on a cold key.
    pub fn document(&mut self, id: &str) -> Result<Snapshot, Error> {
        let mut store = self.host.store.lock().unwrap();
        store.load_keys(
            snap_document::server::TABLES[0],
            &[vec![snap_store::Value::Text(id.into())]]
                .into_iter()
                .collect(),
        )?;
        let snapshot = store.inspect("controller.document", |tx| {
            self.host.document.retained(tx, id)
        })?;
        self.host.pinned.insert(id.into());
        Ok(snapshot)
    }
    pub fn lifecycle(&mut self, id: &str) -> Result<snap_document::lifecycle::Lifecycle, Error> {
        self.host
            .store
            .lock()
            .unwrap()
            .inspect("controller.state", |tx| {
                self.host.document.lifecycle(tx, id)
            })
    }

    pub fn finalize(&mut self, id: &str, key: &str) -> Result<(), Error> {
        let committed = self
            .host
            .store
            .lock()
            .unwrap()
            .run("controller.finalize", |tx| {
                let mut lifecycle = self.host.document.lifecycle(tx, id)?;
                lifecycle.finalizers.remove(key);
                self.host.document.set_lifecycle(tx, id, &lifecycle)
            })?;
        self.host.committed(&committed.changes)?;
        Ok(())
    }
    pub fn inspect<T>(
        &mut self,
        operation: &str,
        read: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.host.store.lock().unwrap().inspect(operation, read)
    }
    pub fn transact<T>(
        &mut self,
        operation: &str,
        handler: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let committed = self.host.store.lock().unwrap().run(operation, handler)?;
        // The running controller is temporarily removed from the registry to
        // borrow its callback. Its own meaningful writes still request a pass.
        for change in &committed.changes {
            let snapshot = self
                .host
                .store
                .lock()
                .unwrap()
                .inspect("controller.change", |tx| {
                    self.host.document.changed(tx, change)
                })?;
            if let Some(snapshot) = snapshot
                && (snapshot.kind == self.kind
                    || self.host.controllers.contains_key(&snapshot.kind))
            {
                self.host.reconcile.insert(snapshot.id.clone(), snapshot);
            }
        }
        self.host.reconcile_residency()?;
        self.host.synchronize(None, None);
        Ok(committed.value)
    }

    pub fn progress(&mut self, value: Value) -> Result<(), Error> {
        if let Some(work) = self.work {
            if let crate::Operation::Request(invocation) = &work.operation
                && !(self.host.requests[&invocation.operation].progress)(&value)
            {
                return Err(Error::Invalid);
            }
            self.host.respond(
                work,
                Event::Progress {
                    id: work.wire_id,
                    value,
                },
            );
        }
        Ok(())
    }
}
