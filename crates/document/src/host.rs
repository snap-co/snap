//! Document's selected participation in host execution. This module interprets
//! Document commits, retains authorized state and reconciles Document controllers.
//! It owns no dispatcher, credentials, operation registry or application runtime.
mod controller;
pub use controller::{Controller, ControllerContext};

use crate::{Completion, Manifest, Replication, ServerMessage, Snapshot, server::Document};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use snap_host::{CommitContext, Participant};
use snap_store::{Backend, Error, RowChange};
use snap_transport::{Response, Value, runtime::Output};

/// Selected by application assembly independently of its Transport operations.
/// Controllers run after desired-state persistence, one blocking pass at a time.
pub struct Documents<B: Backend> {
    state: State,
    controllers: BTreeMap<String, Controller<B>>,
}
struct State {
    document: Arc<Document>,
    kinds: BTreeSet<String>,
    reconcile: BTreeMap<String, Snapshot>,
    residency: BTreeMap<u64, (String, BTreeSet<String>)>,
    held: BTreeMap<u64, BTreeMap<String, Snapshot>>,
    pinned: BTreeSet<String>,
}

/// Document operations declare their post-commit observations. The generic host
/// never decodes Document results or recognizes an operation name.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Publication {
    pub completion: Completion,
    pub replication: Option<Replication>,
}

impl<B: Backend> Documents<B> {
    pub fn new(document: Arc<Document>) -> Self {
        Self {
            state: State {
                document,
                kinds: BTreeSet::new(),
                reconcile: BTreeMap::new(),
                residency: BTreeMap::new(),
                held: BTreeMap::new(),
                pinned: BTreeSet::new(),
            },
            controllers: BTreeMap::new(),
        }
    }
    pub fn with_controller(mut self, kind: &str, controller: Controller<B>) -> Self {
        assert!(
            self.controllers.insert(kind.into(), controller).is_none(),
            "duplicate controller type"
        );
        self.state.kinds.insert(kind.into());
        self
    }
    pub fn residency_references(&self, document: &str) -> usize {
        self.state
            .residency
            .values()
            .filter(|(_, ids)| ids.contains(document))
            .count()
    }
}

impl State {
    fn committed<B: Backend>(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        changes: &[RowChange],
    ) -> Result<(), Error> {
        for change in changes {
            let snapshot = ctx
                .store
                .inspect("controller.change", |tx| self.document.changed(tx, change))?;
            if let Some(snapshot) = snapshot
                && self.kinds.contains(&snapshot.kind)
            {
                self.reconcile.insert(snapshot.id.clone(), snapshot);
            }
        }
        Ok(())
    }

    /// Logical requirements, accepted work, controller dependencies and finalizers
    /// all keep rows resident. Housekeeping cannot narrow an accepted table scan.
    fn residency<B: Backend>(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        for (id, connection) in ctx.connections {
            let entry = self
                .residency
                .entry(*id)
                .or_insert_with(|| (connection.lifetime.clone(), BTreeSet::new()));
            if connection.open {
                entry.1 = ctx.store.inspect("residency.authorized", |tx| {
                    crate::DocumentAccessGuard::new(&self.document).extent(tx, &connection.actor)
                })?;
            }
        }
        let cleanup = ctx
            .store
            .inspect("residency.cleanup", |tx| self.document.cleanup_ids(tx))?;
        let keys = self
            .residency
            .values()
            .flat_map(|(_, ids)| ids.iter())
            .chain(self.pinned.iter())
            .chain(cleanup.iter())
            .chain(self.reconcile.keys())
            .map(|id| vec![snap_store::Value::Text(id.clone())])
            .collect();
        ctx.store.load_keys(crate::server::TABLES[0], &keys)?;
        if !ctx.data.contains(crate::server::TABLES[0]) {
            ctx.store.retain_keys(crate::server::TABLES[0], &keys)?;
        }
        Ok(())
    }

    fn desired<B: Backend>(
        &self,
        ctx: &mut CommitContext<'_, B>,
        peer: u64,
    ) -> Result<Vec<Snapshot>, Error> {
        let peer = ctx.peers.get(&peer).ok_or(Error::NotFound)?;
        let actor = peer.actor().ok_or(Error::Invalid)?;
        let connection = peer
            .connection()
            .and_then(|id| ctx.connections.get(&id))
            .ok_or(Error::NotFound)?;
        if !connection.open {
            return Err(Error::NotFound);
        }
        ctx.store.inspect("document.delivery", |tx| {
            let bearer = peer.bearer().ok_or(Error::NotFound)?;
            // Retained renewable credentials are not current read authority.
            if ctx.authority.identify(tx, bearer)? != actor {
                return Err(Error::NotFound);
            }
            self.document
                .access_guard()
                .manifest(tx, &connection.lifetime, actor, &Manifest::default())
                .map(|m| m.documents)
        })
    }

    fn synchronize<B: Backend>(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        replication: Option<&Replication>,
        origin: Option<u64>,
    ) {
        for id in ctx.peers.keys().copied() {
            let Ok(documents) = self.desired(ctx, id) else {
                continue;
            };
            let peer = &ctx.peers[&id];
            let output = peer.output();
            let desired: BTreeMap<_, _> = documents
                .iter()
                .map(|s| (s.id.clone(), s.clone()))
                .collect();
            authorize_output(output, &desired.keys().cloned().collect());
            let held = self.held.entry(id).or_default();
            let removed: Vec<_> = held
                .keys()
                .filter(|id| !desired.contains_key(*id))
                .cloned()
                .collect();
            if !removed.is_empty() {
                output.push_back(notification(ServerMessage::Removed(removed)));
            }
            let gained = desired.keys().any(|id| !held.contains_key(id));
            let replacement = gained
                || desired.iter().any(|(id, snapshot)| {
                    held.get(id) != Some(snapshot)
                        && replication.is_none_or(|intent| intent.intent.document != *id)
                });
            if replacement {
                output.push_back(notification(ServerMessage::Holdings(documents)));
            } else if let Some(intent) = replication
                && peer.connection() != origin
                && desired.contains_key(&intent.intent.document)
            {
                output.push_back(notification(ServerMessage::Replication(intent.clone())));
            }
            *held = desired;
        }
    }

    fn observed<B: Backend>(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        let changes = ctx.take_changes();
        self.committed(ctx, &changes)?;
        self.residency(ctx)?;
        self.synchronize(ctx, None, None);
        Ok(())
    }
}

impl<B: Backend> Participant<B> for Documents<B> {
    fn prepare(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        ctx.store.load(crate::server::TABLES[0])
    }
    fn maintain(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        let retired: Vec<_> = self
            .state
            .residency
            .keys()
            .filter(|id| !ctx.connections.contains_key(id))
            .copied()
            .collect();
        for id in retired {
            let (lifetime, _) = self.state.residency.remove(&id).expect("known connection");
            // Expired receipts grant no authority and cannot match another boot.
            let _ = ctx.store.run("document.expire", |tx| {
                self.state.document.expire(tx, &lifetime)
            });
            for (peer_id, peer) in ctx.peers {
                if peer.connection() == Some(id) {
                    self.state.held.remove(peer_id);
                    peer.output()
                        .retain(|response| matches!(response, Response::Event(_)));
                    peer.output().push_back(notification(ServerMessage::Reset));
                }
            }
        }
        self.state
            .held
            .retain(|id, _| ctx.peers.get(id).is_some_and(|p| p.connection().is_some()));
        self.state.residency(ctx)
    }
    fn accepted(&mut self, connection: Option<u64>) {
        if let Some(connection) = connection
            && let Some((_, ids)) = self.state.residency.get(&connection)
        {
            self.state.pinned.extend(ids.iter().cloned());
        }
    }
    fn detached(&mut self, peer: u64) {
        self.state.held.remove(&peer);
    }
    fn committed(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        changes: &[RowChange],
        publication: &Value,
    ) -> Result<(), Error> {
        let publication = publication
            .get(crate::wire::KIND)
            .map(|value| {
                serde_json::from_value::<Publication>(value.clone()).map_err(|_| Error::Invalid)
            })
            .transpose()?;
        if let Some(publication) = &publication
            && publication.completion.result.is_ok()
            && let Some(invocation) = &ctx.invocation
        {
            // Retire optimism before any controller holdings, not the call trace.
            invocation.progress.publish(
                crate::wire::KIND,
                serde_json::to_value(ServerMessage::Committed(publication.completion.clone()))
                    .map_err(|_| Error::Invalid)?,
            );
        }
        self.state.committed(ctx, changes)?;
        self.state.residency(ctx)?;
        self.state.synchronize(
            ctx,
            publication.as_ref().and_then(|p| p.replication.as_ref()),
            ctx.invocation.as_ref().and_then(|i| i.connection),
        );
        Ok(())
    }
    fn reconcile(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<bool, Error> {
        let Some((_, snapshot)) = self.state.reconcile.pop_first() else {
            return Ok(false);
        };
        self.state.pinned.insert(snapshot.id.clone());
        let lifecycle = ctx.store.inspect("controller.state", |tx| {
            self.state.document.lifecycle(tx, &snapshot.id)
        })?;
        if lifecycle.blocked.is_some() {
            return if ctx.invocation.is_some() {
                Err(Error::Unavailable)
            } else {
                Ok(true)
            };
        }
        let id = snapshot.id.clone();
        let Some(controller) = self.controllers.get_mut(&snapshot.kind) else {
            return Ok(true);
        };
        let result = controller(
            &mut ControllerContext {
                state: &mut self.state,
                commits: ctx,
            },
            snapshot,
        );
        if let Err(error) = result {
            ctx.transact("controller.blocked", |tx| {
                let mut lifecycle = self.state.document.lifecycle(tx, &id)?;
                lifecycle.blocked = Some(error.to_string());
                self.state.document.set_lifecycle(tx, &id, &lifecycle)
            })?;
            self.state.observed(ctx)?;
            self.state.reconcile.remove(&id);
            return Err(error);
        }
        Ok(true)
    }
    fn recover(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        ctx.store.load(crate::server::TABLES[0])?;
        let snapshots = ctx
            .store
            .inspect("controller.scan", |tx| self.state.document.snapshots(tx))?;
        for snapshot in snapshots {
            if self.state.kinds.contains(&snapshot.kind) {
                self.state.reconcile.insert(snapshot.id.clone(), snapshot);
            }
        }
        Ok(())
    }
    fn release(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        self.state.pinned.clear();
        self.state.residency(ctx)
    }
    fn filter(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        peer: u64,
        response: &mut Response,
    ) -> Result<bool, Error> {
        if let Response::Global { kind, input } = response
            && kind == crate::wire::KIND
        {
            let allowed = if ctx
                .peers
                .get(&peer)
                .and_then(|p| p.connection())
                .is_some_and(|id| ctx.connections.get(&id).is_some_and(|c| c.open))
            {
                Some(
                    self.state
                        .desired(ctx, peer)?
                        .into_iter()
                        .map(|s| s.id)
                        .collect(),
                )
            } else {
                None
            };
            if let Ok(mut message) = serde_json::from_value::<ServerMessage>(input.clone()) {
                if !filter_message(&mut message, allowed.as_ref()) {
                    return Ok(false);
                }
                *input = serde_json::to_value(message).map_err(|_| Error::Invalid)?;
            }
        }
        Ok(true)
    }
}

fn authorize_output(output: &Output, allowed: &BTreeSet<String>) {
    output.retain_mut(|response| {
        if let Response::Global { kind, input } = response
            && kind == crate::wire::KIND
            && let Ok(mut message) = serde_json::from_value::<ServerMessage>(input.clone())
        {
            if !filter_message(&mut message, Some(allowed)) {
                return false;
            }
            *input = serde_json::to_value(message).expect("document message");
        }
        true
    });
}
fn filter_completion(completion: &mut Completion, allowed: Option<&BTreeSet<String>>) {
    if !allowed.is_some_and(|set| set.contains(&completion.document)) && completion.result.is_ok() {
        completion.result = Ok(None);
    }
}
fn filter_message(message: &mut ServerMessage, allowed: Option<&BTreeSet<String>>) -> bool {
    match message {
        ServerMessage::Completed(completion) => filter_completion(completion, allowed),
        ServerMessage::Manifest(state) => {
            state
                .unchanged
                .retain(|h| allowed.is_some_and(|set| set.contains(&h.document)));
            state
                .documents
                .retain(|s| allowed.is_some_and(|set| set.contains(&s.id)));
            for completion in &mut state.completed {
                filter_completion(completion, allowed);
            }
        }
        ServerMessage::Holdings(documents) => {
            documents.retain(|s| allowed.is_some_and(|set| set.contains(&s.id)))
        }
        ServerMessage::Replication(intent) => {
            return allowed.is_some_and(|set| set.contains(&intent.intent.document));
        }
        _ => {}
    }
    true
}
fn notification(message: ServerMessage) -> Response {
    Response::Global {
        kind: crate::wire::KIND.into(),
        input: serde_json::to_value(message).expect("document message"),
    }
}
