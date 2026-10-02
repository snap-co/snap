//! Testy's assembly of module-owned Identity definitions and a bearer provider.
//! The bridge schedules ordinary Transport definitions on the calculator's FIFO.
use snap_identity::{Crypto, Identity};
use snap_store::{Backend, Store};
use snap_transport::{
    Error, Invocation,
    bearer::{Provider, Reply},
    operation::{Context, Runtime},
    server::{Authority, Config, Server},
};
use std::sync::{Arc, Mutex};

struct CryptoHandle<C>(Arc<Mutex<C>>);
impl<C> Clone for CryptoHandle<C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<C: Crypto> Crypto for CryptoHandle<C> {
    fn random(&mut self) -> Result<[u8; 32], snap_store::Error> {
        self.0
            .lock()
            .map_err(|_| snap_store::Error::Unavailable)?
            .random()
    }
    fn hash_password(&mut self, password: &str) -> Result<String, snap_store::Error> {
        self.0
            .lock()
            .map_err(|_| snap_store::Error::Unavailable)?
            .hash_password(password)
    }
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, snap_store::Error> {
        self.0
            .lock()
            .map_err(|_| snap_store::Error::Unavailable)?
            .verify_password(password, hash)
    }
    fn digest(&self, secret: &str) -> Vec<u8> {
        self.0.lock().expect("crypto lock").digest(secret)
    }
}
struct State<B, C> {
    store: Store<B>,
    requests: Runtime<()>,
    provider: Arc<dyn Provider>,
    _crypto: CryptoHandle<C>,
    clock: Box<dyn Fn() -> i64 + Send>,
}
pub struct Sessions<B, C>(Arc<Mutex<State<B, C>>>);
impl<B, C> Clone for Sessions<B, C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<B: Backend, C: Crypto + Send + 'static> Sessions<B, C> {
    /// Store residency is supplied by the host, never loaded inside an operation.
    pub fn new(
        store: Store<B>,
        crypto: C,
        identity: Identity,
        clock: impl Fn() -> i64 + Send + 'static,
    ) -> Self {
        let crypto = CryptoHandle(Arc::new(Mutex::new(crypto)));
        let factory = crypto.clone();
        let operations =
            snap_identity::operation::definitions(identity, move || factory.clone(), None);
        let mut requests = Runtime::default();
        for definition in operations
            .preconnection
            .into_iter()
            .chain(operations.requests)
        {
            requests.register(definition).expect("Identity declaration");
        }
        Self(Arc::new(Mutex::new(State {
            store,
            requests,
            provider: Arc::new(identity.provider(crypto.clone())),
            _crypto: crypto,
            clock: Box::new(clock),
        })))
    }
}
impl<B: Backend + Send + 'static, C: Crypto + Send + 'static> Sessions<B, C> {
    pub fn prepare(
        &self,
        invocation: &Invocation,
        bearer: Option<&str>,
    ) -> Option<snap_transport::execution::PreparedRequest> {
        if !snap_identity::operation::recognizes(&invocation.operation) {
            return None;
        }
        let prepared = (|| {
            let mut state = self.0.lock().map_err(|_| Error::Unavailable)?;
            let State {
                store,
                requests,
                provider,
                clock,
                ..
            } = &mut *state;
            let selection = requests.definitions().resolve(&invocation.operation)?;
            let definition = requests.definitions().get(selection);
            if !(definition.input)(&invocation.input) {
                return Err(Error::InvalidInput);
            }
            let now = clock();
            let principal = if let Some(bearer) = bearer {
                match store.inspect("transport.authenticate", |tx| {
                    provider.identify(tx, bearer, now)
                }) {
                    Ok(principal) => Some(principal),
                    Err(snap_store::Error::NotFound) if !definition.identity_required => None,
                    Err(error) => return Err(snap_transport::operation::storage_error(error)),
                }
            } else {
                None
            };
            requests.enqueue((), invocation.clone(), selection)?;
            let (work, call, selection) = requests.acquire().ok_or(Error::Occupied)?;
            let context = Context {
                actor: principal
                    .as_ref()
                    .map(|principal| principal.identity.clone()),
                principal,
                bearer: bearer.map(str::to_owned),
                inputs: [("clock".into(), snap_transport::json!(now))]
                    .into_iter()
                    .collect(),
                ..Context::default()
            };
            requests
                .accept(store, work, call, selection, context)
                .map_err(|(_, error)| {
                    requests.reject();
                    error
                })
        })();
        if let Err(error) = prepared {
            return Some(Err(error));
        }
        let sessions = self.clone();
        Some(Ok(Box::new(move || {
            let Ok(mut state) = sessions.0.lock() else {
                return Err(Error::Unavailable).into();
            };
            let State {
                store, requests, ..
            } = &mut *state;
            let completed = requests.execute(store).expect("admitted operation");
            let reply = Reply {
                accepted: true,
                outcome: completed.outcome,
                bearer: completed.context.bearer_change,
            };
            requests.finish();
            reply
        })))
    }
}
impl<B: Backend, C: Crypto + Send + 'static> Authority for Sessions<B, C> {
    fn identify(&self, bearer: &str) -> Result<String, Error> {
        let mut state = self.0.lock().map_err(|_| Error::Unavailable)?;
        let State {
            store,
            provider,
            clock,
            ..
        } = &mut *state;
        let now = clock();
        store
            .inspect("transport.authenticate", |tx| {
                provider.identify(tx, bearer, now)
            })
            .map(|principal| principal.identity)
            .map_err(snap_transport::operation::storage_error)
    }
}
pub fn platform<B: Backend + Send + 'static, C: Crypto + Send + 'static>(
    sessions: Sessions<B, C>,
) -> snap_transport::execution::Runtime<testy::App, Sessions<B, C>> {
    let requests = sessions.clone();
    snap_transport::execution::Runtime::new(
        Server::new(
            sessions,
            Config {
                reconnect_ms: 0,
                capacity: 128,
            },
        )
        .with_live_authority(),
        snap_transport::execution::Executor::new(testy::App::default(), 128).unwrap(),
    )
    .with_requests(
        snap_identity::operation::recognizes,
        move |invocation, bearer| requests.prepare(invocation, bearer),
    )
}
pub fn open(
    path: &std::path::Path,
) -> Result<Sessions<snap_store_sqlite::Sqlite, snap_crypto::Native>, Box<dyn std::error::Error>> {
    let mut store = snap_store_sqlite::Sqlite::open(path)?;
    Identity::default().data().prepare(&mut store)?;
    Ok(Sessions::new(
        store,
        snap_crypto::Native,
        Identity::default(),
        || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|time| time.as_secs() as i64)
                .unwrap_or(-1)
        },
    ))
}
