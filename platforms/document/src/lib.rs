//! Store-backed, globally serialized document host. Network adapters only submit
//! commands and drain observations; acceptance and execution are separate steps.
pub mod web;

use snap_document::{
    ClientMessage, Completion, Manifest, ServerMessage, Snapshot, server::Document,
};
use snap_store::{Backend, Store, Transaction};
use snap_transport::server::{Attachment, Authority, Config, Server};
use snap_transport::{Command, Error, Event, Invocation, Response, Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Runs inside the SAME Store transaction as each protected document operation.
pub type Authenticate =
    Arc<dyn Fn(&mut Transaction<'_>, &str) -> Result<String, snap_store::Error> + Send + Sync>;
/// App-owned request operations compose module calls in the supplied transaction.
pub type Requests = Box<
    dyn FnMut(&mut Transaction<'_>, &Invocation, Option<&str>) -> Result<Value, snap_store::Error>
        + Send,
>;

struct StoreAuthority<B> {
    store: Arc<Mutex<Store<B>>>,
    authenticate: Authenticate,
}

impl<B: Backend> Authority for StoreAuthority<B> {
    fn identify(&self, bearer: &str) -> Result<String, Error> {
        self.store
            .lock()
            .unwrap()
            .run("document.authenticate", |tx| {
                (self.authenticate)(tx, bearer)
            })
            .map(|value| value.value)
            .map_err(storage_error)
    }
}

struct Peer {
    attachment: Option<Attachment>,
    bearer: Option<String>,
    actor: Option<String>,
    held: BTreeMap<String, Snapshot>,
    output: VecDeque<Response>,
}

enum Operation {
    Document(ClientMessage),
    Request(Invocation),
}

struct Work {
    peer: u64,
    connection: Option<u64>,
    wire_id: u64,
    bearer: Option<String>,
    actor: Option<String>,
    operation: Operation,
}

pub struct Host<B: Backend> {
    store: Arc<Mutex<Store<B>>>,
    document: Document,
    transport: Server<StoreAuthority<B>>,
    authenticate: Authenticate,
    requests: Option<Requests>,
    peers: BTreeMap<u64, Peer>,
    lifetimes: BTreeSet<u64>,
    queue: VecDeque<Work>,
    next_peer: u64,
    boot: String,
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
        assert!(!boot.is_empty());
        let store = Arc::new(Mutex::new(store));
        let authority = StoreAuthority {
            store: store.clone(),
            authenticate: authenticate.clone(),
        };
        Self {
            store,
            document,
            transport: Server::new(authority, config).with_live_authority(),
            authenticate,
            requests: None,
            peers: BTreeMap::new(),
            lifetimes: BTreeSet::new(),
            queue: VecDeque::new(),
            next_peer: 0,
            boot,
        }
    }

    pub fn with_requests(mut self, requests: Requests) -> Self {
        self.requests = Some(requests);
        self
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
                output: VecDeque::new(),
            },
        );
        Ok(self.next_peer)
    }

    fn lifetime(&self, id: u64) -> String {
        format!("{}:{id}", self.boot)
    }

    pub fn tick(&mut self, now: u64) {
        self.transport.tick(now);
        for connection in self.transport.take_retired() {
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
                    peer.output.clear();
                    peer.output.push_back(notification(ServerMessage::Reset));
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

    pub fn submit(&mut self, peer_id: u64, command: Command, now: u64) -> Result<(), Error> {
        self.tick(now);
        if !self.peers.contains_key(&peer_id) {
            return Err(Error::StaleConnection);
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
                        self.lifetimes.insert(attachment.connection().0);
                        let peer = self.peers.get_mut(&peer_id).unwrap();
                        peer.attachment = Some(attachment);
                        peer.bearer = Some(bearer);
                        peer.actor = actor;
                        Response::Attached { resumed }
                    }
                    Err(error) => Response::Failed(error),
                }
            }
            Command::Invoke(invocation) => {
                if self.queue.len() >= 1024 {
                    return Err(Error::Capacity);
                }
                let attachment = self.peers[&peer_id]
                    .attachment
                    .as_ref()
                    .ok_or(Error::IdentityRequired)?;
                let dispatch = self.transport.invoke(attachment, invocation)?;
                let message = match dispatch.invocation.operation.as_str() {
                    "document.mutate" => ClientMessage::Mutate(
                        serde_json::from_value(dispatch.invocation.input)
                            .map_err(|_| Error::InvalidInput)?,
                    ),
                    "document.manifest" => ClientMessage::Manifest(
                        serde_json::from_value(dispatch.invocation.input)
                            .map_err(|_| Error::InvalidInput)?,
                    ),
                    _ => return Err(Error::UnknownOperation),
                };
                let id = dispatch.invocation.id;
                self.queue.push_back(Work {
                    peer: peer_id,
                    connection: dispatch.connection.map(|id| id.0),
                    wire_id: id,
                    bearer: self.peers[&peer_id].bearer.clone(),
                    actor: dispatch.identity,
                    operation: Operation::Document(message),
                });
                Response::Events(vec![Event::Accepted { id }])
            }
            Command::Request { bearer, invocation } => {
                if self.queue.len() >= 1024 {
                    return Err(Error::Capacity);
                }
                if self.requests.is_none() {
                    return Err(Error::UnknownOperation);
                }
                let id = invocation.id;
                self.queue.push_back(Work {
                    peer: peer_id,
                    connection: None,
                    wire_id: id,
                    bearer,
                    actor: None,
                    operation: Operation::Request(invocation),
                });
                Response::Events(vec![Event::Accepted { id }])
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

    /// Execute at most one globally ordered accepted operation. A socket may submit
    /// its next command after draining ACK, before the host calls this method.
    pub fn step(&mut self) -> bool {
        self.tick(0);
        let Some(work) = self.queue.pop_front() else {
            return false;
        };
        let mut replication = None;
        let outcome = match &work.operation {
            Operation::Request(invocation) => {
                let handler = self.requests.as_mut().expect("queued configured request");
                self.store
                    .lock()
                    .unwrap()
                    .run("application.request", |tx| {
                        handler(tx, invocation, work.bearer.as_deref())
                    })
                    .map(|value| value.value)
                    .map_err(storage_error)
            }
            Operation::Document(message) => {
                let connection = work.connection.expect("connected document operation");
                if !self.lifetimes.contains(&connection) {
                    Err(Error::StaleConnection)
                } else {
                    let lifetime = self.lifetime(connection);
                    let result = self.store.lock().unwrap().run("document.dispatch", |tx| {
                        let actor = (self.authenticate)(
                            tx,
                            work.bearer.as_deref().ok_or(snap_store::Error::Invalid)?,
                        )?;
                        if Some(&actor) != work.actor.as_ref() {
                            return Err(snap_store::Error::Invalid);
                        }
                        match message {
                            ClientMessage::Manifest(manifest) => {
                                let state =
                                    self.document.manifest(tx, &lifetime, &actor, manifest)?;
                                Ok((ServerMessage::Manifest(state), None))
                            }
                            ClientMessage::Mutate(intent) => {
                                let result = self.document.mutate(tx, &lifetime, &actor, intent)?;
                                Ok((
                                    ServerMessage::Completed(result.completion),
                                    result.replication,
                                ))
                            }
                        }
                    });
                    result
                        .map(|committed| {
                            let (message, intent) = committed.value;
                            replication = intent;
                            serde_json::to_value(message).expect("serializable document result")
                        })
                        .map_err(storage_error)
                }
            }
        };
        if let Some(peer) = self.peers.get_mut(&work.peer).filter(|peer| {
            work.connection.is_none()
                || peer.attachment.as_ref().map(|a| a.connection().0) == work.connection
        }) {
            peer.output
                .push_back(Response::Events(vec![Event::Completed {
                    id: work.wire_id,
                    outcome,
                }]));
        }
        // Only committed results reach publication. Read authorization again for
        // every recipient; drain also filters work queued before later revocation.
        self.synchronize(replication.as_ref(), work.connection);
        true
    }

    fn desired(&self, peer: &Peer) -> Result<Vec<Snapshot>, snap_store::Error> {
        let bearer = peer.bearer.as_deref().ok_or(snap_store::Error::Invalid)?;
        let connection = peer
            .attachment
            .as_ref()
            .ok_or(snap_store::Error::Invalid)?
            .connection()
            .0;
        self.store
            .lock()
            .unwrap()
            .run("document.delivery", |tx| {
                let actor = (self.authenticate)(tx, bearer)?;
                if Some(&actor) != peer.actor.as_ref() {
                    return Err(snap_store::Error::Invalid);
                }
                self.document
                    .manifest(tx, &self.lifetime(connection), &actor, &Manifest::default())
                    .map(|m| m.documents)
            })
            .map(|c| c.value)
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
        let value = self.store.lock().unwrap().run(operation, handler)?.value;
        self.synchronize(None, None);
        Ok(value)
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
        self.tick(0);
        let peer = self.peers.get(&peer_id).ok_or(Error::StaleConnection)?;
        if peer.output.is_empty() {
            return Ok(None);
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
                Response::Events(events) => {
                    for event in events {
                        if let Event::Completed {
                            outcome: Ok(input), ..
                        } = event
                            && let Ok(mut message) =
                                serde_json::from_value::<ServerMessage>(input.clone())
                        {
                            filter_message(&mut message, allowed);
                            *input = serde_json::to_value(message).unwrap();
                        }
                    }
                }
                _ => {}
            }
            return Ok(Some(response));
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

pub fn storage_error(error: snap_store::Error) -> Error {
    match error {
        snap_store::Error::Miss(_) => Error::Application(json!({"code":"StoreMiss"})),
        snap_store::Error::Indeterminate | snap_store::Error::Unavailable => Error::Unavailable,
        snap_store::Error::NotFound => Error::InvalidBearer,
        _ => Error::Application(json!({"code":"Rejected"})),
    }
}
