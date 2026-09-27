//! Testy's composition: Identity and transport share one Store authority. Synchronous
//! crypto/transactions run under host exclusion. No session cache or implicit loads.
use snap_identity::{
    Crypto, Identity,
    operation::{Operation, transport_error as error},
};
use snap_store::{Backend, Store};
use snap_transport::{
    Error, Invocation,
    server::{Authority, Config, Server},
};
use std::sync::{Arc, Mutex};

pub struct State<B, C> {
    pub store: Store<B>,
    pub crypto: C,
    pub identity: Identity,
    pub clock: Box<dyn Fn() -> i64 + Send>,
}
pub struct Sessions<B, C>(pub Arc<Mutex<State<B, C>>>);
impl<B, C> Clone for Sessions<B, C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<B: Backend, C: Crypto> Sessions<B, C> {
    pub fn new(
        store: Store<B>,
        crypto: C,
        identity: Identity,
        clock: impl Fn() -> i64 + Send + 'static,
    ) -> Self {
        Self(Arc::new(Mutex::new(State {
            store,
            crypto,
            identity,
            clock: Box::new(clock),
        })))
    }
}
impl<B: Backend + Send + 'static, C: Crypto + Send + 'static> Sessions<B, C> {
    pub fn prepare(
        &self,
        invocation: &Invocation,
        bearer: Option<&str>,
    ) -> Option<snap_platform_local::PreparedRequest> {
        let operation = match Operation::parse(invocation, bearer) {
            Ok(Some(operation)) => operation,
            Ok(None) => return None,
            Err(error) => return Some(Err(error)),
        };
        let sessions = self.clone();
        Some(Ok(Box::new(move || {
            let mut state = sessions.0.lock().map_err(|_| Error::Unavailable)?;
            let State {
                store,
                crypto,
                identity,
                clock,
            } = &mut *state;
            let now = clock();
            store
                .run(operation.name(), |tx| {
                    operation.execute(identity, tx, crypto, now)
                })
                .map(|committed| committed.value)
                .map_err(error)
        })))
    }
}
impl<B: Backend, C: Crypto> Authority for Sessions<B, C> {
    fn identify(&self, bearer: &str) -> Result<String, Error> {
        let mut state = self.0.lock().map_err(|_| Error::Unavailable)?;
        let State {
            store,
            crypto,
            identity,
            clock,
        } = &mut *state;
        let now = clock();
        store
            .run("identity.authority", |tx| {
                identity.resolve(tx, crypto, bearer, now)
            })
            .map(|committed| committed.value.identity)
            .map_err(error)
    }
}
pub fn platform<B: Backend + Send + 'static, C: Crypto + Send + 'static>(
    sessions: Sessions<B, C>,
) -> snap_platform_local::Platform<testy::App, Sessions<B, C>> {
    let requests = sessions.clone();
    snap_platform_local::Platform::new(
        Server::new(
            sessions,
            Config {
                reconnect_ms: 0,
                capacity: 128,
            },
        )
        .with_live_authority(),
        snap_execution::Executor::new(testy::App::default(), 128).unwrap(),
    )
    .with_requests(move |invocation, bearer| requests.prepare(invocation, bearer))
    .ephemeral()
}
pub fn open()
-> Result<Sessions<snap_sqlite::Sqlite, snap_crypto::Native>, Box<dyn std::error::Error>> {
    let path =
        std::env::var("TESTY_DATABASE").unwrap_or_else(|_| ".snap/testy-identity.sqlite".into());
    let mut store = snap_sqlite::Sqlite::open(std::path::Path::new(&path))?;
    for table in snap_identity::TABLES {
        store.load(table)?;
    }
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
