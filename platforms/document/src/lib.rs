//! Store-backed, globally serialized document host. Network adapters only submit
//! commands and drain observations; acceptance and execution are separate steps.
mod controller;
pub mod tcp;
pub mod web;
pub use controller::{Controller, ControllerContext};

use snap_document::{Completion, Manifest, ServerMessage, Snapshot, server::Document};
use snap_store::{Backend, Store, Transaction};
use snap_transport::operation::{Context, Definition, Runtime, Selection, storage_error};
use snap_transport::server::{Attachment, Authority, Config, ConnectionId, Server};
use snap_transport::{Command, Error, Event, Invocation, Response, Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Runs against resident Store state for identification and admission, never
/// external IO. Accepted work retains the actor instead of rerunning this callback
/// during handler execution. Later visibility observations use current authority.
pub type Authenticate =
    Arc<dyn Fn(&mut Transaction<'_>, &str) -> Result<String, snap_store::Error> + Send + Sync>;
type Input = Box<dyn FnMut(&str) -> Result<Value, Error> + Send>;
struct StoreAuthority<B> {
    store: Arc<Mutex<Store<B>>>,
    authenticate: Authenticate,
    lifetime: Authenticate,
}

impl<B: Backend> Authority for StoreAuthority<B> {
    fn identify(&self, bearer: &str) -> Result<String, Error> {
        self.store
            .lock()
            .unwrap()
            .inspect("document.authenticate", |tx| {
                (self.authenticate)(tx, bearer)
            })
            .map_err(storage_error)
    }
    fn retained(&self, bearer: &str) -> Result<String, Error> {
        self.store
            .lock()
            .unwrap()
            .inspect("document.lifetime", |tx| (self.lifetime)(tx, bearer))
            .map_err(storage_error)
    }
}

struct Peer {
    attachment: Option<Attachment>,
    bearer: Option<String>,
    actor: Option<String>,
    held: BTreeMap<String, Snapshot>,
    output: Output,
    control: CarrierControl,
}

/// Physical teardown can be requested while controller IO holds the host gate.
/// The host consumes it before another protected admission. Accepted work drains.
#[derive(Clone, Default)]
pub struct CarrierControl(Arc<Mutex<Option<(bool, u64)>>>);

impl CarrierControl {
    pub fn detach(&self, now: u64) {
        self.0.lock().unwrap().get_or_insert((false, now));
    }

    pub fn close(&self, now: u64) {
        *self.0.lock().unwrap() = Some((true, now));
    }
}

/// Carrier-owned handle. Socket writes and progress draining never acquire the
/// execution gate, including while a synchronous controller is doing host IO.
#[derive(Clone, Default)]
pub struct Output(Arc<Mutex<VecDeque<Response>>>);

impl Output {
    pub fn pop_front(&self) -> Option<Response> {
        self.0.lock().unwrap().pop_front()
    }
    fn push_back(&self, response: Response) {
        self.0.lock().unwrap().push_back(response);
    }
    fn is_empty(&self) -> bool {
        self.0.lock().unwrap().is_empty()
    }
    fn front(&self) -> Option<Response> {
        self.0.lock().unwrap().front().cloned()
    }
    fn retain(&self, keep: impl FnMut(&Response) -> bool) {
        self.0.lock().unwrap().retain(keep);
    }
    fn authorize(&self, allowed: &BTreeSet<String>) {
        self.0.lock().unwrap().retain_mut(|response| {
            if let Response::Notification { input, .. } = response
                && let Ok(mut message) = serde_json::from_value::<ServerMessage>(input.clone())
            {
                if !filter_message(&mut message, Some(allowed)) {
                    return false;
                }
                *input = serde_json::to_value(message).unwrap();
            }
            true
        });
    }
}

struct Call {
    peer: u64,
    operation: Invocation,
    accepted: bool,
    outcome: Option<snap_transport::Outcome>,
}

#[derive(Clone)]
struct Work {
    peer: u64,
    connection: Option<u64>,
    wire_id: u64,
    bearer: Option<String>,
    actor: Option<String>,
    operation: Invocation,
    selection: Selection,
}

pub struct Host<B: Backend> {
    store: Arc<Mutex<Store<B>>>,
    document: Arc<Document>,
    transport: Server<StoreAuthority<B>>,
    authenticate: Authenticate,
    requests: Runtime<Work>,
    input: Input,
    preconnection_requests: BTreeSet<Selection>,
    peers: BTreeMap<u64, Peer>,
    lifetimes: BTreeSet<u64>,
    controllers: BTreeMap<String, Controller<B>>,
    reconcile: BTreeMap<String, Snapshot>,
    calls: BTreeMap<(u64, u64), Call>,
    residency: BTreeMap<u64, (String, BTreeSet<String>)>,
    pinned: BTreeSet<String>,
    next_peer: u64,
    boot: String,
    retention_ms: u64,
}

impl<B: Backend> Host<B> {
    /// `boot` must be a new random namespace each host start. Logical connections
    /// are ephemeral across process loss; old receipts must not identify new work.
    pub fn new(
        store: Store<B>,
        document: Document,
        authenticate: Authenticate,
        config: Config,
        boot: String,
    ) -> Self {
        Self::new_with_lifetime_authority(
            store,
            document,
            authenticate.clone(),
            authenticate,
            config,
            boot,
        )
    }

    /// Separate renewable login lifetime from current access authority. Lifetime
    /// checks govern retained state only; connect, invocation admission and
    /// protected Document publication still use `authenticate`. Neither callback
    /// may do external IO. Local/absolute expiry and known revocation must fail
    /// both, while an owned refresh may preserve state without granting access.
    pub fn new_with_lifetime_authority(
        store: Store<B>,
        document: Document,
        authenticate: Authenticate,
        lifetime: Authenticate,
        config: Config,
        boot: String,
    ) -> Self {
        assert!(!boot.is_empty());
        let store = Arc::new(Mutex::new(store));
        let authority = StoreAuthority {
            store: store.clone(),
            authenticate: authenticate.clone(),
            lifetime,
        };
        let document = Arc::new(document);
        let mut requests = Runtime::default();
        for definition in snap_document::operations::definitions(document.clone()) {
            requests.register(definition).expect("document operation");
        }
        Self {
            store,
            document,
            transport: Server::new(authority, config).with_live_authority(),
            authenticate,
            requests,
            input: Box::new(|_| Err(Error::Unavailable)),
            preconnection_requests: BTreeSet::new(),
            peers: BTreeMap::new(),
            lifetimes: BTreeSet::new(),
            controllers: BTreeMap::new(),
            reconcile: BTreeMap::new(),
            calls: BTreeMap::new(),
            residency: BTreeMap::new(),
            pinned: BTreeSet::new(),
            next_peer: 0,
            boot,
            retention_ms: config.reconnect_ms,
        }
    }

    pub fn with_request(mut self, request: Definition) -> Self {
        assert!(
            self.peers.is_empty() && self.requests.idle(),
            "assemble operations before accepting traffic"
        );
        self.requests
            .register(request)
            .expect("valid, unique operation name");
        self
    }

    /// Bootstrap selects physical providers for explicitly declared read-only
    /// inputs. Called only for the FIFO owner, never inside a portable handler.
    pub fn with_inputs(
        mut self,
        input: impl FnMut(&str) -> Result<Value, Error> + Send + 'static,
    ) -> Self {
        self.input = Box::new(input);
        self
    }

    /// Sensitive acquisition runs as one connectionless exchange. It cannot be
    /// invoked on a retained attachment, and its inputs/results are removed after
    /// completion. Carriers own projection, including HTTP cookies. Validation,
    /// admission, FIFO and durable completion are shared with connected operations.
    pub fn with_preconnection_request(mut self, request: Definition) -> Self {
        let name = request.name.clone();
        self = self.with_request(request);
        self.preconnection_requests
            .insert(self.requests.definitions().resolve(&name).unwrap());
        self
    }

    pub fn is_preconnection_request(&self, name: &str) -> bool {
        self.requests
            .definitions()
            .resolve(name)
            .is_ok_and(|selection| self.preconnection_requests.contains(&selection))
    }
    pub fn retention_ms(&self) -> u64 {
        self.retention_ms
    }
    /// Identifies the exact logical lifetime, including its fresh boot namespace.
    /// A carrier must expose this only after successful authenticated attachment.
    pub fn attachment_lifetime(&self, peer: u64) -> Result<String, Error> {
        let attachment = self
            .peers
            .get(&peer)
            .and_then(|p| p.attachment.as_ref())
            .ok_or(Error::StaleConnection)?;
        Ok(self.lifetime(attachment.connection().0))
    }

    pub fn authorize_upgrade(&self, bearer: &str) -> Result<(), Error> {
        self.store
            .lock()
            .unwrap()
            .inspect("transport.upgrade", |tx| {
                (self.authenticate)(tx, bearer).map(|_| ())
            })
            .map_err(storage_error)
    }

    /// One connectionless exchange; the caller holds the execution mutex on a blocking
    /// thread. Credentials/results live only for this exchange and are removed
    /// from peer and retry storage before returning. No automatic retry.
    pub fn preconnection_request(
        &mut self,
        invocation: Invocation,
        bearer: Option<String>,
    ) -> snap_transport::Outcome {
        let selection = self.requests.definitions().resolve(&invocation.operation)?;
        if !self.preconnection_requests.contains(&selection) {
            return Err(Error::UnknownOperation);
        }
        let request = self.requests.definitions().get(selection);
        if !(request.input)(&invocation.input) {
            return Err(Error::InvalidInput);
        }
        if self.requests.pending() >= 1024 {
            return Err(Error::Capacity);
        }
        let peer = self.open()?;
        let output = self.output(peer)?;
        let id = invocation.id;
        let result = (|| {
            self.enqueue(Work {
                peer,
                connection: None,
                wire_id: id,
                bearer,
                actor: None,
                operation: invocation,
                selection,
            })?;
            loop {
                while let Some(response) = output.pop_front() {
                    if let Response::Events(events) = response {
                        for event in events {
                            if let Event::Completed {
                                id: completed,
                                outcome,
                            } = event
                                && completed == id
                            {
                                return outcome;
                            }
                        }
                    }
                }
                self.step();
            }
        })();
        self.peers.remove(&peer);
        self.calls
            .retain(|(connection, _), _| *connection != (peer | (1 << 63)));
        result
    }

    pub fn with_controller(mut self, kind: &str, controller: Controller<B>) -> Self {
        assert!(
            self.controllers.insert(kind.into(), controller).is_none(),
            "duplicate controller type"
        );
        self
    }

    fn committed(&mut self, changes: &[snap_store::RowChange]) -> Result<(), snap_store::Error> {
        for change in changes {
            let snapshot = self
                .store
                .lock()
                .unwrap()
                .inspect("controller.change", |tx| self.document.changed(tx, change))?;
            if let Some(snapshot) = snapshot
                && self.controllers.contains_key(&snapshot.kind)
            {
                self.reconcile.insert(snapshot.id.clone(), snapshot);
            }
        }
        Ok(())
    }

    /// Union of logical-connection requirements and currently owned work. Socket
    /// loss leaves these references intact until transport reports actual closure.
    fn reconcile_residency(&mut self) -> Result<(), snap_store::Error> {
        let mut store = self.store.lock().unwrap();
        for (connection, (actor, ids)) in &mut self.residency {
            if self.transport.state(ConnectionId(*connection))
                == snap_transport::server::ConnectionState::Open
            {
                *ids = store
                    .run("residency.authorized", |tx| {
                        snap_document::DocumentAccessGuard::new(&self.document).extent(tx, actor)
                    })?
                    .value;
            }
        }
        let cleanup = store.inspect("residency.cleanup", |tx| self.document.cleanup_ids(tx))?;
        let keys = self
            .residency
            .values()
            .flat_map(|(_, ids)| ids.iter())
            .chain(self.pinned.iter())
            .chain(cleanup.iter())
            .chain(self.reconcile.keys())
            .map(|id| vec![snap_store::Value::Text(id.clone())])
            .collect();
        store.load_keys(snap_document::server::TABLES[0], &keys)?;
        if self
            .requests
            .tables()
            .contains(&snap_document::server::TABLES[0])
        {
            Ok(())
        } else {
            store.retain_keys(snap_document::server::TABLES[0], &keys)
        }
    }

    pub fn residency_references(&self, document: &str) -> usize {
        self.residency
            .values()
            .filter(|(_, ids)| ids.contains(document))
            .count()
    }

    fn reconcile(&mut self, work: Option<&Work>) -> Result<(), snap_store::Error> {
        let mut failure = None;
        while let Some((_, snapshot)) = self.reconcile.pop_first() {
            self.pinned.insert(snapshot.id.clone());
            let lifecycle = self
                .store
                .lock()
                .unwrap()
                .inspect("controller.state", |tx| {
                    self.document.lifecycle(tx, &snapshot.id)
                })?;
            if lifecycle.blocked.is_some() {
                if work.is_some() {
                    failure.get_or_insert(snap_store::Error::Unavailable);
                }
                continue;
            }
            let kind = snapshot.kind.clone();
            let id = snapshot.id.clone();
            let Some(mut controller) = self.controllers.remove(&kind) else {
                continue;
            };
            let result = controller(
                &mut ControllerContext {
                    host: self,
                    work,
                    kind: &kind,
                },
                snapshot,
            );
            self.controllers.insert(kind, controller);
            if let Err(error) = result {
                self.store.lock().unwrap().run("controller.blocked", |tx| {
                    let mut lifecycle = self.document.lifecycle(tx, &id)?;
                    lifecycle.blocked = Some(error.to_string());
                    self.document.set_lifecycle(tx, &id, &lifecycle)
                })?;
                self.synchronize(None, None);
                self.reconcile.remove(&id);
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Recover missed in-process notifications by scanning current stored state.
    /// Controllers inspect actual resources and leave explicitly blocked state alone.
    pub fn recover_controllers(&mut self) -> Result<(), snap_store::Error> {
        while self.step() {}
        self.store
            .lock()
            .unwrap()
            .load(snap_document::server::TABLES[0])?;
        let snapshots = self
            .store
            .lock()
            .unwrap()
            .run("controller.scan", |tx| self.document.snapshots(tx))?
            .value;
        for snapshot in snapshots {
            if self.controllers.contains_key(&snapshot.kind) {
                self.reconcile.insert(snapshot.id.clone(), snapshot);
            }
        }
        let result = self.reconcile(None);
        self.pinned.clear();
        self.reconcile_residency()?;
        result
    }

    pub fn open(&mut self) -> Result<u64, Error> {
        if self.peers.len() >= 128 {
            return Err(Error::Capacity);
        }
        self.next_peer = self.next_peer.checked_add(1).ok_or(Error::Capacity)?;
        self.peers.insert(
            self.next_peer,
            Peer {
                attachment: None,
                bearer: None,
                actor: None,
                held: BTreeMap::new(),
                output: Output::default(),
                control: CarrierControl::default(),
            },
        );
        Ok(self.next_peer)
    }

    fn lifetime(&self, id: u64) -> String {
        format!("{}:{id}", self.boot)
    }

    pub fn output(&self, peer: u64) -> Result<Output, Error> {
        self.peers
            .get(&peer)
            .map(|peer| peer.output.clone())
            .ok_or(Error::StaleConnection)
    }

    pub fn tick(&mut self, now: u64) {
        self.apply_carrier_controls();
        self.transport.tick(now);
        let retired = self.transport.take_retired();
        let released = !retired.is_empty();
        for connection in retired {
            self.residency.remove(&connection.0);
            self.calls.retain(|(id, _), _| *id != connection.0);
            self.lifetimes.remove(&connection.0);
            let lifetime = self.lifetime(connection.0);
            // Failure is terminal for this expired connection regardless of cleanup.
            // Orphan receipt rows grant no authority and cannot match a fresh boot.
            let _ = self
                .store
                .lock()
                .unwrap()
                .run("document.expire", |tx| self.document.expire(tx, &lifetime));
            for peer in self.peers.values_mut() {
                if peer
                    .attachment
                    .as_ref()
                    .is_some_and(|a| a.connection() == connection)
                {
                    peer.held.clear();
                    peer.output
                        .retain(|response| matches!(response, Response::Events(_)));
                    peer.output.push_back(notification(ServerMessage::Reset));
                }
            }
        }
        if released
            && !self
                .requests
                .tables()
                .contains(&snap_document::server::TABLES[0])
        {
            let keys = self
                .residency
                .values()
                .flat_map(|(_, ids)| ids.iter())
                .chain(self.pinned.iter())
                .chain(self.reconcile.keys())
                .map(|id| vec![snap_store::Value::Text(id.clone())])
                .collect();
            // A fenced Store already rejects every subsequent read. Releasing
            // memory cannot repair an indeterminate durable outcome.
            let _ = self
                .store
                .lock()
                .unwrap()
                .retain_keys(snap_document::server::TABLES[0], &keys);
        }
    }

    pub fn carrier_control(&self, peer: u64) -> Result<CarrierControl, Error> {
        self.peers
            .get(&peer)
            .map(|peer| peer.control.clone())
            .ok_or(Error::StaleConnection)
    }

    fn apply_carrier_controls(&mut self) {
        let controls: Vec<_> = self
            .peers
            .iter()
            .filter_map(|(id, peer)| {
                peer.control
                    .0
                    .lock()
                    .unwrap()
                    .take()
                    .map(|signal| (*id, signal))
            })
            .collect();
        for (id, (close, now)) in controls {
            if let Some(peer) = self.peers.remove(&id)
                && let Some(attachment) = peer.attachment
            {
                if close {
                    let _ = self.transport.close(&attachment);
                } else {
                    let _ = self.transport.disconnect(&attachment, now);
                }
            }
            self.calls
                .retain(|(connection, _), _| *connection != (id | (1 << 63)));
        }
    }

    pub fn retired(&self, peer: u64) -> bool {
        self.peers.get(&peer).is_none_or(|peer| {
            peer.attachment
                .as_ref()
                .is_some_and(|a| !self.transport.attached(a))
        })
    }

    pub fn submit(&mut self, peer_id: u64, command: Command, now: u64) -> Result<(), Error> {
        self.tick(now);
        if !self.peers.contains_key(&peer_id) {
            return Err(Error::StaleConnection);
        }
        let selection = match &command {
            Command::Invoke(invocation) | Command::Request { invocation, .. } => self
                .requests
                .definitions()
                .resolve(&invocation.operation)
                .ok(),
            _ => None,
        };
        if selection.is_some_and(|selection| self.preconnection_requests.contains(&selection)) {
            return Err(Error::UnknownOperation);
        }
        let response = match command {
            Command::Connect { bearer, client_id } => {
                if self.peers[&peer_id].attachment.is_some() {
                    return Err(Error::Occupied);
                }
                let actor = self
                    .transport
                    .request(
                        Some(&bearer),
                        Invocation {
                            id: 0,
                            operation: String::new(),
                            input: Value::Null,
                        },
                    )?
                    .identity;
                match self.transport.connect(&bearer, &client_id, now) {
                    Ok((attachment, resumed)) => {
                        let connection = attachment.connection().0;
                        self.lifetimes.insert(connection);
                        let peer = self.peers.get_mut(&peer_id).unwrap();
                        peer.attachment = Some(attachment);
                        peer.bearer = Some(bearer);
                        peer.actor = actor;
                        self.residency.insert(
                            connection,
                            (
                                peer.actor.clone().expect("authenticated connection"),
                                BTreeSet::new(),
                            ),
                        );
                        self.reconcile_residency().map_err(storage_error)?;
                        Response::Attached { resumed }
                    }
                    Err(error) => Response::Failed(error),
                }
            }
            Command::Invoke(invocation) => {
                if self.requests.pending() >= 1024 {
                    return Err(Error::Capacity);
                }
                let attachment = self.peers[&peer_id]
                    .attachment
                    .as_ref()
                    .ok_or(Error::IdentityRequired)?;
                if !self.transport.attached(attachment) {
                    return Err(Error::StaleConnection);
                }
                let connection = attachment.connection().0;
                let selection = selection.ok_or(Error::UnknownOperation)?;
                let request = self.requests.definitions().get(selection);
                if !(request.input)(&invocation.input) {
                    return Err(Error::InvalidInput);
                }
                let id = invocation.id;
                self.enqueue(Work {
                    peer: peer_id,
                    connection: Some(connection),
                    wire_id: id,
                    bearer: self.peers[&peer_id].bearer.clone(),
                    actor: self.peers[&peer_id].actor.clone(),
                    operation: invocation,
                    selection,
                })?;
                self.admit_next();
                return Ok(());
            }
            Command::Request { bearer, invocation } => {
                if self.requests.pending() >= 1024 {
                    return Err(Error::Capacity);
                }
                let selection = selection.ok_or(Error::UnknownOperation)?;
                let request = self.requests.definitions().get(selection);
                if !(request.input)(&invocation.input) {
                    return Err(Error::InvalidInput);
                }
                let id = invocation.id;
                self.enqueue(Work {
                    peer: peer_id,
                    connection: None,
                    wire_id: id,
                    bearer,
                    actor: None,
                    operation: invocation,
                    selection,
                })?;
                self.admit_next();
                return Ok(());
            }
            Command::Close | Command::Disconnect => {
                let attachment = self
                    .peers
                    .get_mut(&peer_id)
                    .unwrap()
                    .attachment
                    .take()
                    .ok_or(Error::StaleConnection)?;
                if matches!(command, Command::Close) {
                    self.transport.close(&attachment)?;
                } else {
                    self.transport.disconnect(&attachment, now)?;
                }
                self.tick(now);
                let peer = self.peers.get_mut(&peer_id).unwrap();
                peer.actor = None;
                peer.bearer = None;
                peer.held.clear();
                Response::Detached
            }
        };
        self.peers
            .get_mut(&peer_id)
            .unwrap()
            .output
            .push_back(response);
        Ok(())
    }

    fn call_key(work: &Work) -> (u64, u64) {
        (
            work.connection.unwrap_or(work.peer | (1 << 63)),
            work.wire_id,
        )
    }

    fn enqueue(&mut self, work: Work) -> Result<(), Error> {
        if work.wire_id == 0 {
            return Err(Error::InvalidInput);
        }
        let key = Self::call_key(&work);
        if let Some(call) = self.calls.get_mut(&key) {
            if call.operation != work.operation {
                return Err(Error::Protocol);
            }
            call.peer = work.peer;
            let accepted = call.accepted;
            let outcome = call.outcome.clone();
            if accepted {
                self.respond(&work, Event::Accepted { id: work.wire_id });
            }
            if let Some(outcome) = outcome {
                self.respond(
                    &work,
                    Event::Completed {
                        id: work.wire_id,
                        outcome,
                    },
                );
            }
            return Ok(());
        }
        if self.calls.len() >= 16384 {
            return Err(Error::Capacity);
        }
        self.requests
            .enqueue(work.clone(), work.operation.clone(), work.selection)?;
        self.calls.insert(
            key,
            Call {
                peer: work.peer,
                operation: work.operation.clone(),
                accepted: false,
                outcome: None,
            },
        );
        Ok(())
    }

    fn respond(&mut self, work: &Work, event: Event) {
        if let Some(call) = self.calls.get_mut(&Self::call_key(work)) {
            match &event {
                Event::Accepted { .. } => call.accepted = true,
                Event::Completed { outcome, .. } => call.outcome = Some(outcome.clone()),
                Event::Progress { .. } => {}
            }
        }
        // A retry explicitly reattaches observation interest. Merely reconnecting
        // must not send an old completion into a fresh physical SDK exchange.
        let target = self
            .calls
            .get(&Self::call_key(work))
            .map_or(work.peer, |call| call.peer);
        if let Some(peer) = self.peers.get_mut(&target) {
            peer.output.push_back(Response::Events(vec![event]));
        }
    }

    fn admit_next(&mut self) {
        self.apply_carrier_controls();
        while let Some((mut work, invocation, selection)) = self.requests.acquire() {
            let tables = self.requests.definitions().get(selection).tables;
            let loaded = tables
                .iter()
                .try_for_each(|table| self.store.lock().unwrap().load(table));
            if let Err(error) = loaded {
                self.respond(
                    &work,
                    Event::Completed {
                        id: work.wire_id,
                        outcome: Err(storage_error(error)),
                    },
                );
                self.requests.reject();
                continue;
            }
            let authenticated = self
                .store
                .lock()
                .unwrap()
                .inspect("document.admit", |tx| {
                    if let Some(connection) = work.connection
                        && self.transport.state(ConnectionId(connection))
                            != snap_transport::server::ConnectionState::Open
                    {
                        return Err(snap_store::Error::NotFound);
                    }
                    let actor = work
                        .bearer
                        .as_deref()
                        .map(|bearer| (self.authenticate)(tx, bearer))
                        .transpose()?;
                    if work.connection.is_some() && actor != work.actor {
                        return Err(snap_store::Error::Invalid);
                    }
                    Ok(actor)
                })
                .map_err(storage_error);
            let actor = match authenticated {
                Ok(actor) => actor,
                Err(error) => {
                    self.respond(
                        &work,
                        Event::Completed {
                            id: work.wire_id,
                            outcome: Err(error),
                        },
                    );
                    self.requests.reject();
                    continue;
                }
            };
            work.actor = actor.clone();
            if let Some(connection) = work.connection
                && let Err(error) = self.transport.retain(ConnectionId(connection))
            {
                self.respond(
                    &work,
                    Event::Completed {
                        id: work.wire_id,
                        outcome: Err(error),
                    },
                );
                self.requests.reject();
                continue;
            }
            let mut context = Context {
                actor,
                bearer: work.bearer.clone(),
                lifetime: work.connection.map(|id| self.lifetime(id)),
                ..Context::default()
            };
            let supplied = self
                .requests
                .definitions()
                .get(selection)
                .inputs
                .iter()
                .try_for_each(|key| {
                    context.inputs.insert((*key).into(), (self.input)(key)?);
                    Ok::<_, Error>(())
                });
            if let Err(error) = supplied {
                if let Some(connection) = work.connection {
                    self.transport
                        .release(ConnectionId(connection))
                        .expect("prepared connection pin");
                }
                self.respond(
                    &work,
                    Event::Completed {
                        id: work.wire_id,
                        outcome: Err(error),
                    },
                );
                self.requests.reject();
                continue;
            }
            let result = self.requests.accept(
                &mut self.store.lock().unwrap(),
                work.clone(),
                invocation,
                selection,
                context,
            );
            match result {
                Ok(()) => {
                    self.respond(&work, Event::Accepted { id: work.wire_id });
                    if let Some(connection) = work.connection {
                        self.pinned = self
                            .residency
                            .get(&connection)
                            .map(|(_, ids)| ids.clone())
                            .unwrap_or_default();
                    }
                    return;
                }
                Err((_, error)) => {
                    if let Some(connection) = work.connection {
                        self.transport
                            .release(ConnectionId(connection))
                            .expect("prepared connection pin");
                    }
                    self.respond(
                        &work,
                        Event::Completed {
                            id: work.wire_id,
                            outcome: Err(error),
                        },
                    );
                    self.requests.reject();
                }
            }
        }
    }

    /// Execute one operation with the authority captured before ACK. The active
    /// slot is the gate, including the gap between admission and execution.
    pub fn step(&mut self) -> bool {
        self.admit_next();
        let Some(completed) = self.requests.execute(&mut self.store.lock().unwrap()) else {
            return false;
        };
        let work = completed.work;
        let replication = serde_json::from_value::<Option<snap_document::Replication>>(
            completed.context.publication,
        )
        .unwrap_or(None);
        let changes = completed.changes;
        let outcome = if self.preconnection_requests.contains(&work.selection)
            && let Some(error) = completed.storage_failure
        {
            Err(match error {
                snap_store::Error::Invalid => Error::InvalidInput,
                snap_store::Error::Constraint => Error::Application(json!({"code":"Conflict"})),
                other => storage_error(other),
            })
        } else {
            completed.outcome
        };
        // Publish the desired commit before reconciling its external resources.
        if work.operation.operation == "document.mutate"
            && let Ok(value) = &outcome
            && let Ok(ServerMessage::Completed(completion)) =
                serde_json::from_value::<ServerMessage>(value.clone())
            && completion.result.is_ok()
        {
            // Retire only the optimistic mutation, not the invocation trace.
            // This must precede progress holdings to avoid applying it twice.
            let target = self
                .calls
                .get(&Self::call_key(&work))
                .map_or(work.peer, |call| call.peer);
            if let Some(peer) = self.peers.get(&target) {
                peer.output
                    .push_back(notification(ServerMessage::Committed(completion)));
            }
        }
        let mut reconciled = self
            .committed(&changes)
            .and_then(|()| self.reconcile_residency())
            .and_then(|()| {
                self.synchronize(replication.as_ref(), work.connection);
                self.reconcile(Some(&work))
            });
        let full_documents = self
            .requests
            .tables()
            .contains(&snap_document::server::TABLES[0]);
        self.requests.release_tables();
        if full_documents {
            self.pinned.clear();
            // Restore the current extent after all accepted work and controllers
            // finish. This releases full-table knowledge without readmission.
            reconciled = reconciled.and(self.reconcile_residency());
        }
        let outcome = match (outcome, reconciled) {
            (Ok(_), Err(error)) => Err(Error::Application(
                json!({"code":"Blocked", "committed":true, "cause":error.to_string()}),
            )),
            (outcome, _) => outcome,
        };
        // Only committed results reach publication. Read authorization again for
        // every recipient; drain also filters work queued before later revocation.
        if let Some(connection) = work.connection {
            self.transport
                .release(ConnectionId(connection))
                .expect("accepted operation retains its connection");
        }
        self.pinned.clear();
        self.tick(0);
        // All synchronous work and logical-resource finalization is done. Only
        // now expose the terminal frame to the independent carrier output queue.
        self.respond(
            &work,
            Event::Completed {
                id: work.wire_id,
                outcome,
            },
        );
        self.requests.finish();
        true
    }

    fn desired(&self, peer: &Peer) -> Result<Vec<Snapshot>, snap_store::Error> {
        let actor = peer.actor.as_deref().ok_or(snap_store::Error::Invalid)?;
        let connection = peer
            .attachment
            .as_ref()
            .ok_or(snap_store::Error::Invalid)?
            .connection()
            .0;
        self.store
            .lock()
            .unwrap()
            .inspect("document.delivery", |tx| {
                // A retained renewable login is not current read authority.
                // Do not publish new holdings while its access is unavailable.
                let bearer = peer.bearer.as_deref().ok_or(snap_store::Error::NotFound)?;
                if (self.authenticate)(tx, bearer)? != actor {
                    return Err(snap_store::Error::NotFound);
                }
                self.document
                    .access_guard()
                    .manifest(tx, &self.lifetime(connection), actor, &Manifest::default())
                    .map(|m| m.documents)
            })
    }

    fn synchronize(
        &mut self,
        replication: Option<&snap_document::Replication>,
        origin: Option<u64>,
    ) {
        let peers: Vec<u64> = self.peers.keys().copied().collect();
        for id in peers {
            if self.peers[&id].attachment.is_none() || self.retired(id) {
                continue;
            }
            let Ok(documents) = self.desired(&self.peers[&id]) else {
                continue;
            };
            let peer = self.peers.get_mut(&id).unwrap();
            let desired: BTreeMap<_, _> = documents
                .iter()
                .map(|s| (s.id.clone(), s.clone()))
                .collect();
            peer.output.authorize(&desired.keys().cloned().collect());
            let removed: Vec<_> = peer
                .held
                .keys()
                .filter(|id| !desired.contains_key(*id))
                .cloned()
                .collect();
            if !removed.is_empty() {
                peer.output
                    .push_back(notification(ServerMessage::Removed(removed)));
            }
            let gained = desired.keys().any(|id| !peer.held.contains_key(id));
            let replacement = gained
                || desired.iter().any(|(id, snapshot)| {
                    peer.held.get(id) != Some(snapshot)
                        && replication.is_none_or(|intent| intent.intent.document != *id)
                });
            if replacement {
                peer.output
                    .push_back(notification(ServerMessage::Holdings(documents)));
            } else if let Some(intent) = replication
                && peer.attachment.as_ref().map(|a| a.connection().0) != origin
                && desired.contains_key(&intent.intent.document)
            {
                peer.output
                    .push_back(notification(ServerMessage::Replication(intent.clone())));
            }
            peer.held = desired;
        }
    }

    /// App effects/Access changes share Store and trigger holdings reconciliation.
    /// Callers receive values only after durable commit, as with Store::run.
    pub fn transact<T>(
        &mut self,
        operation: &str,
        handler: impl FnOnce(&mut Transaction<'_>) -> Result<T, snap_store::Error>,
    ) -> Result<T, snap_store::Error> {
        // Bootstrap/internal transactions wait behind accepted and queued work.
        // Public application operations must enter dispatch instead.
        while self.step() {}
        // Explicit bootstrap/internal host IO. Client synchronization never calls
        // this escape hatch; application operations use guarded dispatch.
        self.store
            .lock()
            .unwrap()
            .load(snap_document::server::TABLES[0])?;
        let committed = self.store.lock().unwrap().run(operation, handler)?;
        self.committed(&committed.changes)?;
        self.reconcile_residency()?;
        self.synchronize(None, None);
        self.reconcile(None)?;
        Ok(committed.value)
    }

    pub fn drain(&mut self, peer_id: u64) -> Result<Vec<Response>, Error> {
        let mut output = Vec::new();
        while let Some(response) = self.next_response(peer_id)? {
            output.push(response);
        }
        Ok(output)
    }

    /// Authorize each frame immediately before handing it to carrier IO. A
    /// carrier must not pre-drain a batch across asynchronous socket writes.
    pub fn next_response(&mut self, peer_id: u64) -> Result<Option<Response>, Error> {
        let peer = self.peers.get(&peer_id).ok_or(Error::StaleConnection)?;
        if peer.output.is_empty() {
            return Ok(None);
        }
        if matches!(peer.output.front(), Some(Response::Events(_))) {
            return Ok(self.peers.get_mut(&peer_id).unwrap().output.pop_front());
        }
        let allowed: Option<BTreeSet<String>> =
            if peer.attachment.is_some() && !self.retired(peer_id) {
                Some(
                    self.desired(peer)
                        .map_err(storage_error)?
                        .into_iter()
                        .map(|s| s.id)
                        .collect(),
                )
            } else {
                None
            };
        let peer = self.peers.get_mut(&peer_id).unwrap();
        while let Some(mut response) = peer.output.pop_front() {
            let allowed = allowed.as_ref();
            match &mut response {
                Response::Notification { input, .. } => {
                    if let Ok(mut message) = serde_json::from_value::<ServerMessage>(input.clone())
                    {
                        if !filter_message(&mut message, allowed) {
                            continue;
                        }
                        *input = serde_json::to_value(message).unwrap();
                    }
                }
                // Invocation results retain their accepted authority. Only
                // ongoing Document synchronization uses current authorization.
                Response::Events(_) => {}
                _ => {}
            }
            return Ok(Some(response));
        }
        Ok(None)
    }

    pub fn lost(&mut self, peer: u64, now: u64) {
        self.calls.retain(|(id, _), _| *id != (peer | (1 << 63)));
        if let Some(peer) = self.peers.remove(&peer)
            && let Some(attachment) = peer.attachment
        {
            let _ = self.transport.disconnect(&attachment, now);
        }
        self.tick(now);
    }
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
            documents.retain(|s| allowed.is_some_and(|set| set.contains(&s.id)));
        }
        ServerMessage::Replication(intent) => {
            return allowed.is_some_and(|set| set.contains(&intent.intent.document));
        }
        _ => {}
    }
    true
}

fn notification(message: ServerMessage) -> Response {
    Response::Notification {
        operation: "document".into(),
        input: serde_json::to_value(message).unwrap(),
    }
}
