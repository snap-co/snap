//! Opt-in whole-record Store replication. Table declarations supply current read
//! policy, usually through Access. Hosts own subscriptions and physical delivery.
//! No declaration means no export. The private durable program is never sent.
//!
//! This spike loads declared tables in full on the server. Clients request exact
//! keys or module-declared collections, never arbitrary queries. Reconnect gets a
//! fresh snapshot, not log catch-up. Collections are reselected after every commit.
//! Foreign keys and indexes stay authoritative-server constraints; the replica
//! catalog contains only exported table shapes. No partial-column export yet.
use crate::host::{CommitContext, Participant};
use crate::{
    Operation, Response, Value,
    operation::{Definition, TypedFailure},
};
use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::Arc,
    vec::Vec,
};
use snap_store::{
    Backend, Catalog, Data, Error, Instruction, Program, Row, RowChange, Table, Transaction,
    replica::Publication, resource::Resource,
};

pub const TOPIC: &str = "store.replication";
const SUBSCRIBE: &str = "store.subscribe";
type Authorize =
    dyn Fn(&mut Transaction<'_>, &str, &[snap_store::Value]) -> Result<bool, Error> + Send + Sync;
type Select = dyn Fn(&mut Transaction<'_>, &str, &Value) -> Result<BTreeSet<Vec<snap_store::Value>>, Error>
    + Send
    + Sync;

/// Runtime declaration attached to an exported table, separate from SQL DDL.
/// Every column is exported. Put private material in non-exported tables.
pub struct Declaration {
    pub table: Table,
    pub data: Data,
    authorize: Box<Authorize>,
    select: Option<Box<Select>>,
}
impl Declaration {
    pub fn new(
        table: Table,
        data: Data,
        authorize: impl Fn(&mut Transaction<'_>, &str, &[snap_store::Value]) -> Result<bool, Error>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            table,
            data,
            authorize: Box::new(authorize),
            select: None,
        }
    }
    /// Parameters describe module-owned interest, not SQL or client authority.
    /// The host intersects every selected key with the table's live read policy.
    pub fn with_collection(
        mut self,
        select: impl Fn(
            &mut Transaction<'_>,
            &str,
            &Value,
        ) -> Result<BTreeSet<Vec<snap_store::Value>>, Error>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.select = Some(Box::new(select));
        self
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
/// Requested record interest, not a statement of what the client already holds.
/// Only full rows are supported; listing stubs and payload residency are separate.
pub struct Subscription {
    pub tables: BTreeMap<String, BTreeSet<Vec<snap_store::Value>>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub collections: BTreeMap<String, Value>,
}

pub struct Subscribe;
impl Operation for Subscribe {
    const NAME: &'static str = SUBSCRIBE;
    type Input = Subscription;
    type Output = ();
    type Error = ();
    type Progress = ();
}

pub struct Registry {
    declarations: Vec<Declaration>,
    catalog: Catalog,
    data: Data,
}
impl Registry {
    pub fn new(declarations: Vec<Declaration>) -> Result<Self, Error> {
        let tables = declarations
            .iter()
            .map(|d| {
                let mut table = d.table.clone();
                table.foreign.clear();
                table.indexes.clear();
                table
            })
            .collect();
        let catalog = Catalog::new(tables)?;
        let data = declarations
            .iter()
            .fold(Data::default(), |data, d| data.and(d.data.clone()));
        Ok(Self {
            declarations,
            catalog,
            data,
        })
    }
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Accepted subscriptions publish after the Store transaction succeeds.
    /// Caller assertions never substitute for live authentication or table policy.
    pub fn operation(self: &Arc<Self>) -> Definition {
        let registry = self.clone();
        Definition::typed::<Subscribe>(
            true,
            Vec::new(),
            self.data.clone(),
            &[],
            move |tx, manifest, context| {
                let actor = context.actor.as_deref().ok_or(Error::NotFound)?;
                let lifetime = context.lifetime.as_deref().ok_or(Error::Invalid)?;
                if manifest.tables.len() > registry.declarations.len()
                    || manifest.tables.values().map(BTreeSet::len).sum::<usize>() > 4096
                {
                    return Err(Error::Invalid.into());
                }
                for (table, keys) in &manifest.tables {
                    let declaration = registry
                        .declarations
                        .iter()
                        .find(|d| d.table.name == *table)
                        .ok_or(Error::Invalid)?;
                    for key in keys {
                        Program::from_instructions(
                            &registry.catalog,
                            [Instruction::Delete {
                                table: table.clone(),
                                key: key.clone(),
                            }],
                        )?;
                        if !(declaration.authorize)(tx, actor, key)? {
                            return Err(Error::NotFound.into());
                        }
                    }
                }
                if manifest.collections.len() > registry.declarations.len() {
                    return Err(Error::Invalid.into());
                }
                for (table, parameters) in &manifest.collections {
                    let declaration = registry
                        .declarations
                        .iter()
                        .find(|d| d.table.name == *table)
                        .ok_or(Error::Invalid)?;
                    registry.keys(tx, declaration, actor, Some(parameters), None)?;
                }
                context.publication =
                    crate::json!({SUBSCRIBE: {"lifetime":lifetime,"subscription":manifest}});
                Ok::<_, TypedFailure<()>>(())
            },
        )
    }
    fn keys(
        &self,
        tx: &mut Transaction<'_>,
        declaration: &Declaration,
        actor: &str,
        parameters: Option<&Value>,
        exact: Option<&BTreeSet<Vec<snap_store::Value>>>,
    ) -> Result<BTreeSet<Vec<snap_store::Value>>, Error> {
        let mut candidates = exact.cloned().unwrap_or_default();
        if let Some(parameters) = parameters {
            candidates.extend(declaration.select.as_ref().ok_or(Error::Invalid)?(
                tx, actor, parameters,
            )?);
        }
        if candidates.len() > 4096 {
            return Err(Error::Invalid);
        }
        let mut permitted = BTreeSet::new();
        for key in candidates {
            Program::from_instructions(
                &self.catalog,
                [Instruction::Delete {
                    table: declaration.table.name.clone(),
                    key: key.clone(),
                }],
            )?;
            if (declaration.authorize)(tx, actor, &key)? {
                permitted.insert(key);
            }
        }
        Ok(permitted)
    }
}

type Holdings = BTreeMap<Resource, Row>;
struct Observer {
    rows: Holdings,
    sequence: u64,
}

/// One subscription owner per execution host. Programs flow through the same
/// participant notifications as operation, internal and controller commits.
pub struct Replications {
    registry: Arc<Registry>,
    desired: BTreeMap<String, Subscription>,
    ready: BTreeSet<u64>,
    observers: BTreeMap<u64, Observer>,
    sessions: BTreeSet<String>,
}
impl Replications {
    pub fn new(registry: Arc<Registry>) -> Self {
        Self {
            registry,
            desired: BTreeMap::new(),
            ready: BTreeSet::new(),
            observers: BTreeMap::new(),
            sessions: BTreeSet::new(),
        }
    }

    fn snapshot<B: Backend>(
        &self,
        ctx: &mut CommitContext<'_, B>,
        peer: u64,
    ) -> Result<Holdings, Error> {
        let peer = ctx.peers.get(&peer).ok_or(Error::NotFound)?;
        let Some(connection) = peer
            .connection()
            .and_then(|id| ctx.connections.get(&id))
            .filter(|c| c.open)
        else {
            return Ok(Holdings::new());
        };
        let Some(manifest) = self.desired.get(&connection.lifetime) else {
            return Ok(Holdings::new());
        };
        let actor = &connection.actor;
        ctx.store.inspect("store.replication.authorize", |tx| {
            if peer.actor() != Some(actor.as_str())
                || peer
                    .bearer()
                    .and_then(|b| ctx.authority.identify(tx, b).ok())
                    .as_deref()
                    != Some(actor.as_str())
            {
                tx.status()?;
                return Ok(Holdings::new());
            }
            let mut rows = Holdings::new();
            for declaration in &self.registry.declarations {
                let keys = self.registry.keys(
                    tx,
                    declaration,
                    actor,
                    manifest.collections.get(&declaration.table.name),
                    manifest.tables.get(&declaration.table.name),
                )?;
                for key in keys {
                    if let Some(row) = tx.get(&declaration.table.name, &key)? {
                        rows.insert(Resource::new(&declaration.table.name, &key), row);
                    }
                }
            }
            Ok(rows)
        })
    }

    fn checkpoint(&self, rows: &Holdings) -> Result<Program, Error> {
        Program::from_instructions(
            &self.registry.catalog,
            rows.iter().map(|(resource, row)| Instruction::Insert {
                table: resource.table.clone(),
                row: row.clone(),
            }),
        )
    }

    fn residency<B: Backend>(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        let live: BTreeSet<_> = ctx
            .connections
            .values()
            .map(|c| c.lifetime.clone())
            .collect();
        for expired in self.sessions.difference(&live) {
            for declaration in &self.registry.declarations {
                ctx.residency.remove(
                    &declaration.table.name,
                    &alloc::format!("store.replication:{expired}"),
                );
            }
        }
        for connection in ctx.connections.values() {
            for declaration in &self.registry.declarations {
                let manifest = self.desired.get(&connection.lifetime);
                let permitted = ctx.store.inspect("store.replication.residency", |tx| {
                    self.registry.keys(
                        tx,
                        declaration,
                        &connection.actor,
                        manifest.and_then(|m| m.collections.get(&declaration.table.name)),
                        manifest.and_then(|m| m.tables.get(&declaration.table.name)),
                    )
                })?;
                ctx.residency.set(
                    &declaration.table.name,
                    alloc::format!("store.replication:{}", connection.lifetime),
                    permitted,
                );
            }
        }
        self.sessions = live;
        Ok(())
    }

    fn synchronize<B: Backend>(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        force: Option<u64>,
    ) -> Result<(), Error> {
        let result = self.synchronize_inner(ctx, force);
        if result.is_err() {
            self.redact(ctx)?;
        }
        result
    }

    /// Unavailable policy/data cannot leave an old authorized backlog available
    /// to independently draining carriers. Fail closed with an empty checkpoint.
    fn redact<B: Backend>(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        let program = self.checkpoint(&Holdings::new())?;
        for peer in &self.ready {
            if let Some(connection) = ctx.peers.get(peer) {
                let output = connection.output();
                output.retain(|r| !matches!(r, Response::Global { kind, .. } if kind == TOPIC));
                let sequence = self
                    .observers
                    .get(peer)
                    .map_or(Ok(1), |p| p.sequence.checked_add(1).ok_or(Error::Invalid))?;
                let input = serde_json::to_value(Publication {
                    sequence,
                    reset: true,
                    program: program.as_bytes().to_vec(),
                })
                .map_err(|_| Error::Invalid)?;
                output.push_back(Response::Global {
                    kind: TOPIC.into(),
                    input,
                });
                self.observers.insert(
                    *peer,
                    Observer {
                        rows: Holdings::new(),
                        sequence,
                    },
                );
            }
        }
        Ok(())
    }

    fn synchronize_inner<B: Backend>(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        force: Option<u64>,
    ) -> Result<(), Error> {
        self.prepare(ctx)?;
        self.residency(ctx)?;
        let peers: Vec<_> = ctx
            .peers
            .keys()
            .copied()
            .filter(|p| self.ready.contains(p))
            .collect();
        for peer in peers {
            let rows = self.snapshot(ctx, peer)?;
            let previous = self.observers.get(&peer);
            let reset =
                force == Some(peer) || previous.is_none_or(|p| !p.rows.keys().eq(rows.keys()));
            let program = if reset {
                self.checkpoint(&rows)?
            } else if let Some(program) = &ctx.program {
                let instructions = program.instructions().filter(|instruction| {
                    let Ok(table) = self.registry.catalog.table(instruction.table()) else {
                        return false;
                    };
                    let key = match instruction {
                        Instruction::Insert { row, .. } => table.key(row),
                        Instruction::Update { key, .. } | Instruction::Delete { key, .. } => {
                            key.clone()
                        }
                    };
                    rows.contains_key(&Resource::new(&table.name, &key))
                });
                Program::from_instructions(&self.registry.catalog, instructions)?
            } else {
                // Reconciliation without a supplied program uses a checkpoint.
                if previous.is_some_and(|p| p.rows == rows) {
                    continue;
                }
                self.checkpoint(&rows)?
            };
            let reset = reset || ctx.program.is_none();
            if !reset && program.is_empty() {
                continue;
            }
            let sequence =
                previous.map_or(Ok(1), |p| p.sequence.checked_add(1).ok_or(Error::Invalid))?;
            let output = ctx.peers[&peer].output();
            if reset {
                // Access loss must redact queued bytes before independent carriers
                // drain. A checkpoint replaces that backlog without a sequence gap.
                output.retain(|r| !matches!(r, Response::Global { kind, .. } if kind == TOPIC));
            }
            let input = serde_json::to_value(Publication {
                sequence,
                reset,
                program: program.as_bytes().to_vec(),
            })
            .map_err(|_| Error::Invalid)?;
            output.push_back(Response::Global {
                kind: TOPIC.into(),
                input,
            });
            self.observers.insert(peer, Observer { rows, sequence });
        }
        Ok(())
    }
}

impl<B: Backend> Participant<B> for Replications {
    fn prepare(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        snap_store::resource::data().prepare(ctx.store)?;
        self.registry.data.prepare(ctx.store)?;
        for declaration in &self.registry.declarations {
            let mut table = ctx.store.catalog().table(&declaration.table.name)?.clone();
            table.foreign.clear();
            table.indexes.clear();
            if &table != self.registry.catalog.table(&table.name)? {
                return Err(Error::Invalid);
            }
            ctx.store.load(&table.name)?;
        }
        Ok(())
    }
    fn maintain(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        self.desired
            .retain(|lifetime, _| ctx.connections.values().any(|c| &c.lifetime == lifetime));
        self.synchronize(ctx, None)
    }
    fn detached(&mut self, peer: u64) {
        self.ready.remove(&peer);
        self.observers.remove(&peer);
    }
    fn committed(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        _: &[RowChange],
        publication: &Value,
    ) -> Result<(), Error> {
        let mut force = None;
        if let Some(value) = publication.get(SUBSCRIBE) {
            let lifetime = value["lifetime"].as_str().ok_or(Error::Invalid)?;
            let manifest: Subscription = serde_json::from_value(value["subscription"].clone())
                .map_err(|_| Error::Invalid)?;
            let invocation = ctx.invocation.as_ref().ok_or(Error::Invalid)?;
            if let Some(connection) = invocation
                .connection
                .and_then(|id| ctx.connections.get(&id))
                && connection.lifetime == lifetime
                && ctx
                    .peers
                    .get(&invocation.peer)
                    .is_some_and(|p| p.connection() == invocation.connection)
            {
                self.desired.insert(lifetime.into(), manifest);
                self.ready.insert(invocation.peer);
                force = Some(invocation.peer);
            }
        }
        self.synchronize(ctx, force)
    }
    fn recover(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        self.prepare(ctx)
    }
    fn release(&mut self, ctx: &mut CommitContext<'_, B>) -> Result<(), Error> {
        let result = self.prepare(ctx).and_then(|()| self.residency(ctx));
        if result.is_err() {
            self.redact(ctx)?;
        }
        result
    }
    fn filter(
        &mut self,
        ctx: &mut CommitContext<'_, B>,
        peer: u64,
        response: &mut Response,
    ) -> Result<bool, Error> {
        if let Response::Global { kind, input } = response
            && kind == TOPIC
        {
            // Residency may have narrowed the tables since publication. A
            // collection must be reselected from current complete server data.
            self.prepare(ctx)?;
            let mut publication: Publication =
                serde_json::from_value(input.clone()).map_err(|_| Error::Invalid)?;
            let rows = self.snapshot(ctx, peer)?;
            let program = Program::from_bytes(&self.registry.catalog, &publication.program)?;
            let denied = program.instructions().any(|instruction| {
                let table = self
                    .registry
                    .catalog
                    .table(instruction.table())
                    .expect("validated table");
                let key = match instruction {
                    Instruction::Insert { row, .. } => table.key(&row),
                    Instruction::Update { key, .. } | Instruction::Delete { key, .. } => key,
                };
                !rows.contains_key(&Resource::new(&table.name, &key))
            });
            if denied {
                publication.reset = true;
                publication.program = self.checkpoint(&rows)?.as_bytes().to_vec();
                *input = serde_json::to_value(publication).map_err(|_| Error::Invalid)?;
            }
        }
        Ok(true)
    }
}
