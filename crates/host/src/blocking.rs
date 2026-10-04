use crate::{CommitContext, Connection, InvocationScope, Participant, Progress};
use alloc::{
    boxed::Box,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use snap_store::{Backend, Store, Transaction};
use snap_transport::operation::{
    Context, Registry, Runtime as Operations, Selection, storage_error,
};
use snap_transport::runtime::{CarrierControl, Output};
use snap_transport::server::{Attachment, Authority, Config, ConnectionId, Server};
use snap_transport::{Command, Error, Event, Invocation, Response, Value, json};
use spin::Mutex;

type Input = Box<dyn FnMut(&str) -> Result<Value, Error> + Send>;
struct StoreAuthority<B> {
    store: Arc<Mutex<Store<B>>>,
    authority: Arc<dyn snap_transport::bearer::Authority>,
}

impl<B: Backend> Authority for StoreAuthority<B> {
    fn identify(&self, bearer: &str) -> Result<String, Error> {
        self.store
            .lock()
            .inspect("transport.authenticate", |tx| {
                self.authority.identify(tx, bearer)
            })
            .map_err(storage_error)
    }
    fn retained(&self, bearer: &str) -> Result<String, Error> {
        self.store
            .lock()
            .inspect("transport.lifetime", |tx| {
                self.authority.retained(tx, bearer)
            })
            .map_err(storage_error)
    }
}

pub struct Peer {
    attachment: Option<Attachment>,
    bearer: Option<String>,
    actor: Option<String>,
    output: Output,
    control: CarrierControl,
}

#[derive(Clone)]
struct Work {
    connection: Option<u64>,
    wire_id: u64,
    bearer: Option<String>,
    actor: Option<String>,
    operation: Invocation,
    selection: Selection,
    output: Output,
}

/// A fully blocking engine, driven by its host through `tick` and `step`. It
/// opens no listeners and creates no workers. One FIFO owner retains execution
/// through admission, commit, all controller passes, cleanup and completion.
/// Participants receive Store access and observation capabilities, never dispatch
/// access. Replacing this engine's scheduling does not change their behavior.
pub struct Blocking<B: Backend, P: Participant<B> = ()> {
    store: Arc<Mutex<Store<B>>>,
    participant: P,
    transport: Server<StoreAuthority<B>>,
    authority: Arc<dyn snap_transport::bearer::Authority>,
    requests: Operations<Work>,
    input: Input,
    peers: BTreeMap<u64, Peer>,
    connections: BTreeMap<u64, Connection>,
    next_peer: u64,
    boot: String,
    retention_ms: u64,
}

impl<B: Backend, P: Participant<B>> Blocking<B, P> {
    /// `boot` must be a new random namespace each host start. Logical connections
    /// are ephemeral across process loss; old receipts must not identify new work.
    /// Hosts supply credential policy and prepare its resident data before use.
    /// Application assembly supplies operations and commit participation explicitly.
    /// No module is implicitly registered or required, including Document.
    pub fn new(
        store: Store<B>,
        participant: P,
        operations: Registry,
        authority: Arc<dyn snap_transport::bearer::Authority>,
        config: Config,
        boot: String,
    ) -> Self {
        assert!(!boot.is_empty());
        let store = Arc::new(Mutex::new(store));
        let transport_authority = StoreAuthority {
            store: store.clone(),
            authority: authority.clone(),
        };
        let requests = Operations::new(operations);
        Self {
            store,
            participant,
            transport: Server::new(transport_authority, config).with_live_authority(),
            authority,
            requests,
            input: Box::new(|_| Err(Error::Unavailable)),
            peers: BTreeMap::new(),
            connections: BTreeMap::new(),
            next_peer: 0,
            boot,
            retention_ms: config.reconnect_ms,
        }
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

    pub fn is_preconnection_request(&self, name: &str) -> bool {
        self.requests
            .definitions()
            .resolve(name)
            .is_ok_and(|selection| self.requests.definitions().is_preconnection(selection))
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
            .inspect("transport.upgrade", |tx| {
                self.authority.identify(tx, bearer).map(|_| ())
            })
            .map_err(storage_error)
    }

    /// One connectionless exchange; the caller holds the execution mutex on a blocking
    /// thread. Credentials/results live only for this exchange and are removed
    /// from peer storage before returning. No automatic retry.
    pub fn preconnection_request(
        &mut self,
        invocation: Invocation,
        bearer: Option<String>,
    ) -> snap_transport::Outcome {
        self.preconnection_reply(invocation, bearer).outcome
    }
    pub fn preconnection_reply(
        &mut self,
        invocation: Invocation,
        bearer: Option<String>,
    ) -> snap_transport::bearer::Reply {
        self.private_request(invocation, bearer)
            .unwrap_or_else(|error| Err(error).into())
    }
    fn private_request(
        &mut self,
        invocation: Invocation,
        bearer: Option<String>,
    ) -> Result<snap_transport::bearer::Reply, Error> {
        let selection = self.requests.definitions().resolve(&invocation.operation)?;
        if !self.requests.definitions().is_preconnection(selection) {
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
            let mut bearer_change = None;
            let mut accepted = false;
            self.enqueue(Work {
                connection: None,
                wire_id: id,
                bearer,
                actor: None,
                operation: invocation,
                selection,
                output: output.clone(),
            })?;
            loop {
                while let Some(response) = output.pop_front() {
                    // Private exchanges drain acceptance, credential handoff and
                    // completion separately, with one event per frame.
                    if let Response::Event(event) = response {
                        if matches!(&event, Event::Accepted { id: admitted } if *admitted == id) {
                            accepted = true;
                        }
                        if let Event::Bearer {
                            id: changed,
                            change,
                        } = &event
                            && *changed == id
                        {
                            bearer_change = Some(change.clone());
                        }
                        if let Event::Completed {
                            id: completed,
                            outcome,
                        } = event
                            && completed == id
                        {
                            return Ok(snap_transport::bearer::Reply {
                                accepted,
                                outcome,
                                bearer: bearer_change,
                            });
                        }
                    }
                }
                self.step();
            }
        })();
        self.peers.remove(&peer);
        result
    }

    /// Configure selected participation before handing the engine to its driver.
    pub fn participant(&self) -> &P {
        &self.participant
    }
    pub fn map_participant<Q: Participant<B>>(
        self,
        configure: impl FnOnce(P) -> Q,
    ) -> Blocking<B, Q> {
        assert!(self.requests.idle(), "configure only while idle");
        Blocking {
            store: self.store,
            participant: configure(self.participant),
            transport: self.transport,
            authority: self.authority,
            requests: self.requests,
            input: self.input,
            peers: self.peers,
            connections: self.connections,
            next_peer: self.next_peer,
            boot: self.boot,
            retention_ms: self.retention_ms,
        }
    }

    /// Recover missed notifications from persisted state, then finish every
    /// synchronous pass before traffic can enter the next admission.
    pub fn recover(&mut self) -> Result<(), snap_store::Error> {
        while self.step() {}
        let mut store = self.store.lock();
        let mut context = CommitContext::new(
            &mut store,
            &self.connections,
            &self.peers,
            self.authority.as_ref(),
            self.requests.data(),
            None,
        );
        let result = self
            .participant
            .recover(&mut context)
            .and_then(|()| settle(&mut self.participant, &mut context));
        let released = self.participant.release(&mut context);
        result.and(released)
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
            self.connections.remove(&connection.0);
        }
        for (id, connection) in &mut self.connections {
            connection.open = self.transport.state(ConnectionId(*id))
                == snap_transport::server::ConnectionState::Open;
        }
        if released {
            let _ = self.maintain();
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
            .filter_map(|(id, peer)| peer.control.take().map(|signal| (*id, signal)))
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
        }
    }

    pub fn retired(&self, peer: u64) -> bool {
        self.peers.get(&peer).is_none_or(|peer| {
            peer.attachment
                .as_ref()
                .is_some_and(|a| !self.transport.attached(a))
        })
    }

    /// Invocation IDs correlate observations, not execution identity. Each
    /// submission enters admission independently; this host caches no results
    /// and never redirects an old invocation's output to a replacement peer.
    /// Duplicate handling belongs to the selected operation's guards or handler.
    /// Admission may publish Accepted here. Newly accepted work completes when
    /// step is driven; admission failures can publish here.
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
        if selection
            .is_some_and(|selection| self.requests.definitions().is_preconnection(selection))
        {
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
                        let peer = self.peers.get_mut(&peer_id).unwrap();
                        peer.attachment = Some(attachment);
                        peer.bearer = Some(bearer);
                        peer.actor = actor;
                        self.connections.insert(
                            connection,
                            Connection {
                                actor: peer.actor.clone().expect("authenticated connection"),
                                lifetime: self.lifetime(connection),
                                open: true,
                            },
                        );
                        self.maintain().map_err(storage_error)?;
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
                    connection: Some(connection),
                    wire_id: id,
                    bearer: self.peers[&peer_id].bearer.clone(),
                    actor: self.peers[&peer_id].actor.clone(),
                    operation: invocation,
                    selection,
                    output: self.peers[&peer_id].output.clone(),
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
                    connection: None,
                    wire_id: id,
                    bearer,
                    actor: None,
                    operation: invocation,
                    selection,
                    output: self.peers[&peer_id].output.clone(),
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
                Response::Detached
            }
        };
        let attachment = if matches!(response, Response::Attached { .. }) {
            Some(snap_transport::carrier::AttachmentInfo {
                retention_ms: self.retention_ms(),
                lifetime: self.attachment_lifetime(peer_id)?,
            })
        } else {
            None
        };
        self.peers
            .get_mut(&peer_id)
            .unwrap()
            .output
            .push_frame(snap_transport::carrier::Frame {
                handshake: matches!(response, Response::Attached { .. } | Response::Failed(_)),
                response,
                attachment,
                terminal: false,
            });
        Ok(())
    }

    fn enqueue(&mut self, work: Work) -> Result<(), Error> {
        self.requests
            .enqueue(work.clone(), work.operation.clone(), work.selection)
    }

    fn respond(&mut self, work: &Work, event: Event) {
        work.output.push_back(Response::Event(event));
    }

    fn admit_next(&mut self) {
        self.apply_carrier_controls();
        while let Some((mut work, invocation, selection)) = self.requests.acquire() {
            let data = &self.requests.definitions().get(selection).data;
            let loaded = data.prepare(&mut self.store.lock());
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
                .inspect("transport.admit.identity", |tx| {
                    if let Some(connection) = work.connection
                        && self.transport.state(ConnectionId(connection))
                            != snap_transport::server::ConnectionState::Open
                    {
                        return Err(snap_store::Error::NotFound);
                    }
                    let resolved = self.authority.resolve(
                        tx,
                        work.bearer.as_deref(),
                        work.connection.is_some()
                            || self.requests.definitions().get(selection).identity_required,
                    )?;
                    if work.connection.is_some() && resolved.actor != work.actor {
                        return Err(snap_store::Error::Invalid);
                    }
                    Ok((resolved.actor, resolved.principal))
                })
                .map_err(storage_error);
            let (actor, principal) = match authenticated {
                Ok(facts) => facts,
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
                principal,
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
                &mut self.store.lock(),
                work.clone(),
                invocation,
                selection,
                context,
            );
            match result {
                Ok(()) => {
                    self.respond(&work, Event::Accepted { id: work.wire_id });
                    self.participant.accepted(work.connection);
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
        let Some(completed) = self.requests.execute(&mut self.store.lock()) else {
            return false;
        };
        let work = completed.work;
        let bearer_change = completed.context.bearer_change;
        let publication = completed.context.publication;
        let changes = completed.changes;
        let outcome = if self.requests.definitions().is_preconnection(work.selection)
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
        let progress = Progress::new(
            work.output.clone(),
            work.wire_id,
            self.requests.definitions().get(work.selection).progress,
        );
        let scope = InvocationScope {
            connection: work.connection,
            progress: &progress,
        };
        // A failed transaction has no persisted changes and triggers no controllers.
        // Post-commit failures report the committed outcome; they never roll it back.
        let reconciled = if outcome.is_ok() {
            let mut store = self.store.lock();
            let mut context = CommitContext::new(
                &mut store,
                &self.connections,
                &self.peers,
                self.authority.as_ref(),
                self.requests.data(),
                Some(scope),
            );
            self.participant
                .committed(&mut context, &changes, &publication)
                .and_then(|()| settle(&mut self.participant, &mut context))
        } else {
            Ok(())
        };
        self.requests.release_data();
        let released = {
            let mut store = self.store.lock();
            let mut context = CommitContext::new(
                &mut store,
                &self.connections,
                &self.peers,
                self.authority.as_ref(),
                self.requests.data(),
                None,
            );
            self.participant.release(&mut context)
        };
        let reconciled = reconciled.and(released);
        let outcome = match (outcome, reconciled) {
            (Ok(_), Err(error)) => Err(Error::Application(
                json!({"code":"Blocked", "committed":true, "cause":error.to_string()}),
            )),
            (outcome, _) => outcome,
        };
        if let Some(connection) = work.connection {
            self.transport
                .release(ConnectionId(connection))
                .expect("accepted operation retains its connection");
        }
        self.tick(0);
        // All synchronous work and logical-resource finalization is done. Only
        // now expose the terminal frame to the independent carrier output queue.
        if outcome.is_ok()
            && let Some(change) = bearer_change
        {
            self.respond(
                &work,
                Event::Bearer {
                    id: work.wire_id,
                    change,
                },
            );
        }
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

    fn maintain(&mut self) -> Result<(), snap_store::Error> {
        let mut store = self.store.lock();
        let mut context = CommitContext::new(
            &mut store,
            &self.connections,
            &self.peers,
            self.authority.as_ref(),
            self.requests.data(),
            None,
        );
        self.participant.maintain(&mut context)
    }

    /// Bootstrap/internal transactions wait for the FIFO, then notify selected
    /// participation and drain controller passes before returning the value.
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
        let mut store = self.store.lock();
        let mut context = CommitContext::new(
            &mut store,
            &self.connections,
            &self.peers,
            self.authority.as_ref(),
            self.requests.data(),
            None,
        );
        let result = (|| {
            self.participant.prepare(&mut context)?;
            let committed = context.store.run(operation, handler)?;
            self.participant
                .committed(&mut context, &committed.changes, &Value::Null)?;
            settle(&mut self.participant, &mut context)?;
            Ok(committed.value)
        })();
        let released = self.participant.release(&mut context);
        result.and_then(|value| released.map(|()| value))
    }

    pub fn drain(&mut self, peer_id: u64) -> Result<Vec<Response>, Error> {
        let mut output = Vec::new();
        while let Some(response) = self.next_response(peer_id)? {
            output.push(response);
        }
        Ok(output)
    }

    /// Direct-host observation applies the selected module's delivery filter.
    /// Native carriers drain independent Output handles; modules also reauthorize
    /// queued publications when committed access changes are synchronized.
    pub fn next_response(&mut self, peer_id: u64) -> Result<Option<Response>, Error> {
        let peer = self.peers.get(&peer_id).ok_or(Error::StaleConnection)?;
        if peer.output.is_empty() {
            return Ok(None);
        }
        if matches!(peer.output.front(), Some(Response::Event(_))) {
            return Ok(self.peers.get_mut(&peer_id).unwrap().output.pop_front());
        }
        let mut store = self.store.lock();
        let mut context = CommitContext::new(
            &mut store,
            &self.connections,
            &self.peers,
            self.authority.as_ref(),
            self.requests.data(),
            None,
        );
        while let Some(mut response) = peer.output.pop_front() {
            if self
                .participant
                .filter(&mut context, peer_id, &mut response)
                .map_err(storage_error)?
            {
                return Ok(Some(response));
            }
        }
        Ok(None)
    }

    pub fn lost(&mut self, peer: u64, now: u64) {
        if let Some(peer) = self.peers.remove(&peer)
            && let Some(attachment) = peer.attachment
        {
            let _ = self.transport.disconnect(&attachment, now);
        }
        self.tick(now);
    }
}

impl<B: Backend, P: Participant<B>> snap_store::Host for Blocking<B, P> {
    fn transact<T>(
        &mut self,
        operation: &str,
        handler: impl FnOnce(&mut Transaction<'_>) -> Result<T, snap_store::Error>,
    ) -> Result<T, snap_store::Error> {
        Blocking::transact(self, operation, handler)
    }
}

impl<B: Backend, P: Participant<B>> snap_transport::runtime::Loop for Blocking<B, P> {
    fn open(&mut self) -> Result<u64, Error> {
        Blocking::open(self)
    }
    fn output(&self, peer: u64) -> Result<Output, Error> {
        Blocking::output(self, peer)
    }
    fn carrier_control(&self, peer: u64) -> Result<CarrierControl, Error> {
        Blocking::carrier_control(self, peer)
    }
    fn tick(&mut self, now: u64) {
        Blocking::tick(self, now);
    }
    fn step(&mut self) -> bool {
        Blocking::step(self)
    }
    fn submit(&mut self, peer: u64, command: Command, now: u64) -> Result<(), Error> {
        Blocking::submit(self, peer, command, now)
    }
    fn retired(&self, peer: u64) -> bool {
        Blocking::retired(self, peer)
    }
    fn authorize_upgrade(&self, bearer: &str) -> Result<(), Error> {
        Blocking::authorize_upgrade(self, bearer)
    }
    fn is_preconnection_request(&self, name: &str) -> bool {
        Blocking::is_preconnection_request(self, name)
    }
    fn preconnection_reply(
        &mut self,
        invocation: Invocation,
        bearer: Option<String>,
    ) -> snap_transport::bearer::Reply {
        Blocking::preconnection_reply(self, invocation, bearer)
    }
}

fn settle<B: Backend, P: Participant<B>>(
    participant: &mut P,
    context: &mut CommitContext<'_, B>,
) -> Result<(), snap_store::Error> {
    // A failed controller does not stop independent pending passes. Retain the
    // first error, drain accepted reconciliation, then report it to the caller.
    let mut failure = None;
    loop {
        let changes = context.take_changes();
        if !changes.is_empty() {
            if let Err(error) = participant.committed(context, &changes, &Value::Null) {
                failure.get_or_insert(error);
            }
        }
        match participant.reconcile(context) {
            Ok(true) => {}
            Ok(false) if !context.has_changes() => break,
            Ok(false) => {}
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

impl Peer {
    pub fn connection(&self) -> Option<u64> {
        self.attachment.as_ref().map(|a| a.connection().0)
    }
    pub fn actor(&self) -> Option<&str> {
        self.actor.as_deref()
    }
    pub fn bearer(&self) -> Option<&str> {
        self.bearer.as_deref()
    }
    pub fn output(&self) -> &Output {
        &self.output
    }
}
