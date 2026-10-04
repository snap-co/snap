use super::{CommitContext, Participant};
use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use snap_store::{Backend, Error, RowChange};
use snap_transport::{
    Response, Value,
    subscription::{Definition, Extent},
};

/// Generic physical-observer bookkeeping. Definitions supply only domain extent,
/// state and replication rules; logical residency and queue ownership stay here.
pub struct Subscriptions {
    definitions: Vec<Definition>,
    sessions: BTreeMap<u64, String>,
    held: BTreeMap<(usize, u64), Value>,
}
impl Subscriptions {
    pub fn new(definitions: Vec<Definition>) -> Self {
        let mut topics = alloc::collections::BTreeSet::new();
        for definition in &definitions {
            assert!(
                topics.insert(&definition.topic),
                "duplicate publication topic"
            );
        }
        Self {
            definitions,
            sessions: BTreeMap::new(),
            held: BTreeMap::new(),
        }
    }
    fn reader(index: usize, connection: u64) -> String {
        format!("subscription:{index}:{connection}")
    }
    fn residency<B: Backend>(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        for (id, connection) in ctx.connections {
            self.sessions.insert(*id, connection.lifetime.clone());
            if connection.open {
                for (index, definition) in self.definitions.iter().enumerate() {
                    let keys = ctx.store.inspect("subscription.extent", |tx| {
                        (definition.extent)(tx, &connection.actor)
                    })?;
                    ctx.residency
                        .set(definition.table, Self::reader(index, *id), keys);
                }
            }
        }
        ctx.residency.apply(ctx.store, ctx.data)
    }
    fn allowed<B: Backend>(
        definition: &Definition,
        ctx: &mut CommitContext<'_, B>,
        id: u64,
    ) -> Result<Extent, Error> {
        let peer = ctx.peers.get(&id).ok_or(Error::NotFound)?;
        let actor = peer.actor().ok_or(Error::NotFound)?;
        let connection = peer
            .connection()
            .and_then(|id| ctx.connections.get(&id))
            .ok_or(Error::NotFound)?;
        if !connection.open {
            return Err(Error::NotFound);
        }
        ctx.store.inspect("subscription.authorize", |tx| {
            if ctx
                .authority
                .identify(tx, peer.bearer().ok_or(Error::NotFound)?)?
                != actor
            {
                return Err(Error::NotFound);
            }
            (definition.extent)(tx, actor)
        })
    }
    fn synchronize<B: Backend>(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        publication: &Value,
    ) -> Result<(), Error> {
        let mut failure = None;
        for (index, definition) in self.definitions.iter().enumerate() {
            let data = publication.get(&definition.topic).unwrap_or(&Value::Null);
            for id in ctx.peers.keys().copied() {
                let allowed = Self::allowed(definition, ctx, id);
                let peer = &ctx.peers[&id];
                let output = peer.output();
                // Denied or unavailable live read authority cannot preserve an old
                // publication backlog. Captured-authority invocation Events survive.
                let empty = Extent::new();
                output.retain_mut(|response| {
                    if let Response::Global { kind, input } = response
                        && *kind == definition.topic
                    {
                        return (definition.filter)(input, allowed.as_ref().unwrap_or(&empty))
                            .unwrap_or(false);
                    }
                    true
                });
                let Ok(_) = allowed else {
                    // Queued state is no longer a delivery baseline after it was
                    // redacted. Renewal on this attachment needs a fresh snapshot.
                    self.held.remove(&(index, id));
                    continue;
                };
                let Some(connection) = peer.connection().and_then(|id| ctx.connections.get(&id))
                else {
                    continue;
                };
                let state = ctx.store.inspect("subscription.state", |tx| {
                    (definition.read)(tx, &connection.lifetime, &connection.actor)
                });
                let state = match state {
                    Ok(state) => state,
                    Err(error) => {
                        failure.get_or_insert(error);
                        continue;
                    }
                };
                let prior = self.held.get(&(index, id)).unwrap_or(&Value::Null);
                let origin = ctx
                    .invocation
                    .as_ref()
                    .is_some_and(|invocation| invocation.connection == peer.connection());
                match (definition.changes)(prior, &state, data, origin) {
                    Ok(messages) => {
                        for input in messages {
                            output.push_back(Response::Global {
                                kind: definition.topic.clone(),
                                input,
                            });
                        }
                        self.held.insert((index, id), state);
                    }
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
impl<B: Backend> Participant<B> for Subscriptions {
    fn prepare(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        for definition in &self.definitions {
            definition.data.prepare(ctx.store)?;
            ctx.store.load(definition.table)?;
        }
        Ok(())
    }
    fn maintain(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        let retired: Vec<_> = self
            .sessions
            .keys()
            .filter(|id| !ctx.connections.contains_key(id))
            .copied()
            .collect();
        for id in retired {
            let lifetime = self
                .sessions
                .remove(&id)
                .expect("known subscription lifetime");
            for (index, definition) in self.definitions.iter().enumerate() {
                let _ = ctx.store.run("subscription.expire", |tx| {
                    (definition.expire)(tx, &lifetime)
                });
                ctx.residency
                    .remove(definition.table, &Self::reader(index, id));
                for (peer_id, peer) in ctx.peers {
                    if peer.connection() == Some(id) {
                        self.held.remove(&(index, *peer_id));
                        peer.output().retain(|response| !matches!(response, Response::Global {kind,..} if *kind == definition.topic));
                        peer.output().push_back(Response::Global {
                            kind: definition.topic.clone(),
                            input: definition.reset.clone(),
                        });
                    }
                }
            }
        }
        self.residency(ctx)
    }
    fn accepted(&mut self, ctx: &mut CommitContext<'_, B>, connection: Option<u64>) {
        if let Some(connection) = connection {
            for (index, definition) in self.definitions.iter().enumerate() {
                ctx.residency
                    .pin_reader(definition.table, &Self::reader(index, connection));
            }
        }
    }
    fn detached(&mut self, peer: u64) {
        self.held.retain(|(_, id), _| *id != peer);
    }
    fn committed(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        _: &[RowChange],
        publication: &Value,
    ) -> Result<(), Error> {
        let mut failure = None;
        for definition in &self.definitions {
            let data = publication.get(&definition.topic).unwrap_or(&Value::Null);
            match (definition.origin)(data) {
                Ok(Some(mut input)) => {
                    if let Some(invocation) = &ctx.invocation {
                        let peer = invocation.peer;
                        let same_attachment = ctx.peers.get(&peer).is_some_and(|p| {
                            invocation.connection.is_some()
                                && p.connection() == invocation.connection
                        });
                        let allowed = if same_attachment {
                            Self::allowed(definition, ctx, peer).unwrap_or_default()
                        } else {
                            Extent::new()
                        };
                        // Independent carriers can drain immediately. Filter
                        // before enqueue, not only in the later backlog sweep.
                        if (definition.filter)(&mut input, &allowed).unwrap_or(false) {
                            ctx.invocation
                                .as_ref()
                                .expect("invocation remains captured")
                                .progress
                                .publish(&definition.topic, input);
                        }
                    }
                }
                Err(error) => {
                    failure.get_or_insert(error);
                }
                _ => {}
            }
        }
        let resident = self.residency(ctx);
        let synchronized = self.synchronize(ctx, publication);
        failure.map_or(Ok(()), Err).and(resident).and(synchronized)
    }
    fn recover(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        self.prepare(ctx)
    }
    fn release(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        self.residency(ctx)
    }
    fn filter(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        peer: u64,
        response: &mut Response,
    ) -> Result<bool, Error> {
        if let Response::Global { kind, input } = response
            && let Some((index, definition)) = self
                .definitions
                .iter()
                .enumerate()
                .find(|(_, d)| d.topic == *kind)
        {
            let allowed = match Self::allowed(definition, ctx, peer) {
                Ok(allowed) => allowed,
                Err(_) => {
                    self.held.remove(&(index, peer));
                    Extent::new()
                }
            };
            return (definition.filter)(input, &allowed);
        }
        Ok(true)
    }
}
