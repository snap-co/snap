//! Blocking reconciliation over Store resources, independent of any domain module.
use crate::{CommitContext, Participant, Subscriptions};
use alloc::{
    boxed::Box,
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};
use snap_store::{Backend, Error, Row, RowChange, Transaction, resource::Resource};
use snap_transport::Value;

type Callback<B> =
    dyn FnMut(&mut ControllerContext<'_, '_, B>, Resource) -> Result<(), Error> + Send;
pub struct Controller<B: Backend> {
    name: String,
    table: &'static str,
    select: Box<dyn Fn(&Row) -> bool + Send>,
    run: Box<Callback<B>>,
}
impl<B: Backend> Controller<B> {
    /// The row predicate selects resources, not authority. The host pins and loads
    /// selected keys and delivers current state; repeated notifications coalesce.
    pub fn new(
        name: &str,
        table: &'static str,
        select: impl Fn(&Row) -> bool + Send + 'static,
        run: impl FnMut(&mut ControllerContext<'_, '_, B>, Resource) -> Result<(), Error>
        + Send
        + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            table,
            select: Box::new(select),
            run: Box::new(run),
        }
    }
}
struct Watch {
    table: &'static str,
    select: Box<dyn Fn(&Row) -> bool + Send>,
}
#[derive(Default)]
struct Pending {
    watches: Vec<Watch>,
    resources: BTreeMap<(usize, Resource), ()>,
}
impl Pending {
    fn changed<B: Backend>(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        changes: &[RowChange],
    ) -> Result<(), Error> {
        for change in changes {
            let resource = Resource::changed(change)?;
            if !self.watches.iter().any(|w| w.table == resource.table) {
                continue;
            }
            ctx.store.load_keys(
                &resource.table,
                &[resource.key.clone()].into_iter().collect(),
            )?;
            let row = ctx.store.inspect("controller.select", |tx| {
                tx.get(&resource.table, &resource.key)
            })?;
            let Some(row) = row else {
                continue;
            };
            for (index, watch) in self.watches.iter().enumerate() {
                if watch.table == resource.table && (watch.select)(&row) {
                    ctx.residency.pin(&resource);
                    self.resources.insert((index, resource.clone()), ());
                }
            }
        }
        Ok(())
    }
    fn recover<B: Backend>(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        for (index, watch) in self.watches.iter().enumerate() {
            ctx.store.load(watch.table)?;
            let primary = ctx.store.catalog().table(watch.table)?.primary.clone();
            let rows = ctx
                .store
                .inspect("controller.scan", |tx| tx.find(watch.table, "primary", &[]))?;
            for row in rows {
                if !(watch.select)(&row) {
                    continue;
                }
                let key = primary
                    .iter()
                    .map(|name| row.get(name).cloned().ok_or(Error::Invalid))
                    .collect::<Result<Vec<_>, _>>()?;
                let resource = Resource::new(watch.table, &key);
                ctx.residency.pin(&resource);
                self.resources.insert((index, resource), ());
            }
        }
        Ok(())
    }
}

/// Composition of generic controller reconciliation and selected publication
/// behavior. It holds no Document definitions or document-specific callbacks.
pub struct Controllers<B: Backend, P> {
    participant: P,
    pending: Pending,
    names: Vec<String>,
    callbacks: Vec<Box<Callback<B>>>,
}
impl<B: Backend> Controllers<B, Subscriptions> {
    pub fn new(subscriptions: Vec<snap_transport::subscription::Definition>) -> Self {
        Self::around(Subscriptions::new(subscriptions))
    }
}
impl<B: Backend, P> Controllers<B, P> {
    pub fn around(participant: P) -> Self {
        Self {
            participant,
            pending: Default::default(),
            names: Vec::new(),
            callbacks: Vec::new(),
        }
    }
    pub fn with_controller(mut self, controller: Controller<B>) -> Self {
        assert!(
            !self.names.contains(&controller.name),
            "duplicate controller name"
        );
        self.names.push(controller.name);
        self.pending.watches.push(Watch {
            table: controller.table,
            select: controller.select,
        });
        self.callbacks.push(controller.run);
        self
    }
}
impl<B: Backend, P: Participant<B>> Participant<B> for Controllers<B, P> {
    fn prepare(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        if !self.pending.watches.is_empty() {
            snap_store::resource::data().prepare(ctx.store)?;
            for watch in &self.pending.watches {
                ctx.store.load(watch.table)?;
            }
        }
        self.participant.prepare(ctx)
    }
    fn maintain(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        if !self.pending.watches.is_empty() {
            snap_store::resource::data().prepare(ctx.store)?;
        }
        self.participant.maintain(ctx)
    }
    fn accepted(&mut self, ctx: &mut CommitContext<'_, B>, connection: Option<u64>) {
        self.participant.accepted(ctx, connection);
    }
    fn detached(&mut self, peer: u64) {
        self.participant.detached(peer);
    }
    fn committed(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        changes: &[RowChange],
        publication: &Value,
    ) -> Result<(), Error> {
        // Dispatch declares application data only. Prepare host-owned metadata
        // before selecting passes, including the first connectionless commit.
        let prepared = if self.pending.watches.is_empty() {
            Ok(())
        } else {
            snap_store::resource::data().prepare(ctx.store)
        };
        let selected = prepared.and_then(|()| self.pending.changed(ctx, changes));
        let observed = self.participant.committed(ctx, changes, publication);
        selected.and(observed)
    }
    fn reconcile(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<bool, Error> {
        let Some(((index, resource), ())) = self.pending.resources.pop_first() else {
            return self.participant.reconcile(ctx);
        };
        let lifecycle = ctx
            .store
            .inspect("controller.lifecycle", |tx| resource.lifecycle(tx))?;
        if lifecycle.blocked.is_some() {
            return if ctx.invocation.is_some() {
                Err(Error::Unavailable)
            } else {
                Ok(true)
            };
        }
        ctx.residency.pin(&resource);
        let mut context = ControllerContext {
            commits: ctx,
            pending: &mut self.pending,
            participant: &mut self.participant,
        };
        if let Err(error) = (self.callbacks[index])(&mut context, resource.clone()) {
            context.transact("controller.blocked", |tx| {
                let mut lifecycle = resource.lifecycle(tx)?;
                lifecycle.blocked = Some(error.to_string());
                resource.set_lifecycle(tx, &lifecycle)
            })?;
            self.pending.resources.remove(&(index, resource));
            return Err(error);
        }
        Ok(true)
    }
    fn recover(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        if !self.pending.watches.is_empty() {
            snap_store::resource::data().prepare(ctx.store)?;
        }
        let recovered = self.pending.recover(ctx);
        let observed = self.participant.recover(ctx);
        recovered.and(observed)
    }
    fn release(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        let released = self.participant.release(ctx);
        let resident = ctx.residency.apply(ctx.store, ctx.data);
        released.and(resident)
    }
    fn filter(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        peer: u64,
        response: &mut snap_transport::Response,
    ) -> Result<bool, Error> {
        self.participant.filter(ctx, peer, response)
    }
}

/// Transactions, explicit related-row loading and original-invocation progress.
/// No executor, dispatch registry, physical peer mutation or whole-runtime borrow.
pub struct ControllerContext<'a, 'host, B: Backend> {
    commits: &'a mut CommitContext<'host, B>,
    pending: &'a mut Pending,
    participant: &'a mut dyn Participant<B>,
}
impl<B: Backend> ControllerContext<'_, '_, B> {
    /// Explicit host loading pins a related resource through every current pass.
    pub fn row(&mut self, resource: &Resource) -> Result<Row, Error> {
        self.commits.store.load_keys(
            &resource.table,
            &[resource.key.clone()].into_iter().collect(),
        )?;
        self.commits.residency.pin(resource);
        self.inspect("controller.row", |tx| {
            tx.get(&resource.table, &resource.key)?
                .ok_or(Error::NotFound)
        })
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
        let changes = self.commits.take_changes();
        let selected = self.pending.changed(self.commits, &changes);
        let observed = self
            .participant
            .committed(self.commits, &changes, &Value::Null);
        selected.and(observed)?;
        Ok(value)
    }
    pub fn finalize(&mut self, resource: &Resource, key: &str) -> Result<(), Error> {
        self.transact("controller.finalize", |tx| resource.finalize(tx, key))
    }
    pub fn progress(&mut self, value: Value) -> Result<(), Error> {
        if let Some(invocation) = &self.commits.invocation {
            invocation.progress.send(value)?;
        }
        Ok(())
    }
}
