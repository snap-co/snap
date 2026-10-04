//! Controlled server-role command adapter around the production host. No fake
//! dispatch, acceptance, completion or retry cache lives in this adapter.
use snap_host::Blocking as Host;
use snap_platform_tests::dispatch::{CommitFault, Loss, Platform};
use snap_platform_tests::memory::Memory;
use snap_platform_tests::memory::{CommitRejection, RejectOnce};
use snap_store::{Backend, Catalog, Store};
use snap_transport::{Command, Event, Invocation, Response, server::Config};
#[path = "../../support/host.rs"]
mod assembly;
use assembly::migrations;

pub struct Setup<B: Backend> {
    host: Host<B>,
    peer: u64,
    now: u64,
    rejection: Option<CommitRejection>,
    // Store and its connection must drop before the directory.
    _directory: Option<tempfile::TempDir>,
}

pub fn memory() -> Setup<Memory> {
    let catalog = migrations()
        .iter()
        .try_fold(Catalog::default(), |catalog, migration| {
            migration.apply(&catalog)
        })
        .unwrap();
    setup(
        Store::new(catalog.clone(), Memory::new(catalog).unwrap()).unwrap(),
        None,
    )
}

pub fn rejecting_memory() -> Setup<RejectOnce<Memory>> {
    let catalog = migrations()
        .iter()
        .try_fold(Catalog::default(), |catalog, migration| {
            migration.apply(&catalog)
        })
        .unwrap();
    let (backend, rejection) = RejectOnce::new(Memory::new(catalog.clone()).unwrap());
    let mut setup = setup(Store::new(catalog, backend).unwrap(), None);
    setup.rejection = Some(rejection);
    setup
}
pub fn sqlite_memory() -> Setup<snap_store_sqlite::Sqlite> {
    setup(
        snap_store_sqlite::Sqlite::memory(&migrations()).unwrap(),
        None,
    )
}
pub fn sqlite_file() -> Setup<snap_store_sqlite::Sqlite> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dispatch.sqlite");
    snap_store_sqlite::migrate(&path, &migrations()).unwrap();
    setup(
        snap_store_sqlite::Sqlite::open(&path).unwrap(),
        Some(directory),
    )
}
fn setup<B: Backend>(store: Store<B>, directory: Option<tempfile::TempDir>) -> Setup<B> {
    let mut host = assembly::mount(
        store,
        Config {
            reconnect_ms: 10,
            capacity: 8,
        },
        "platform-properties".into(),
    )
    .unwrap();
    let peer = host.open().unwrap();
    host.submit(
        peer,
        Command::Connect {
            bearer: "alice".into(),
            client_id: "probe-client".into(),
        },
        0,
    )
    .unwrap();
    assert_eq!(
        host.drain(peer).unwrap(),
        vec![Response::Attached { resumed: false }]
    );
    Setup {
        host,
        peer,
        now: 0,
        rejection: None,
        _directory: directory,
    }
}

impl CommitFault for Setup<RejectOnce<Memory>> {
    fn reject_next_commit(&mut self) {
        self.rejection.as_ref().unwrap().arm();
    }
}

impl<B: Backend> Platform for Setup<B> {
    fn call(&mut self, invocation: Invocation) -> Result<(), snap_transport::Error> {
        self.host
            .submit(self.peer, Command::Invoke(invocation), self.now)
    }
    fn events(&mut self) -> Vec<Event> {
        // One event per frame now, so a drain is already flat. Anything else is
        // a handshake or refusal, which this fixture never queues mid-assertion.
        self.host
            .drain(self.peer)
            .unwrap()
            .into_iter()
            .map(|response| match response {
                Response::Event(event) => event,
                other => panic!("unexpected response: {other:?}"),
            })
            .collect()
    }
    fn finish(&mut self) {
        for _ in 0..=1024 {
            if !self.host.step() {
                return;
            }
        }
        panic!("host did not drain its bounded operation queue");
    }
    fn lose(&mut self, loss: Loss) {
        if matches!(loss, Loss::Close) {
            self.host
                .submit(self.peer, Command::Close, self.now)
                .unwrap();
        }
        self.host.lost(self.peer, self.now);
        if matches!(loss, Loss::Expire) {
            self.now += 10;
            self.host.tick(self.now);
        }
    }
    fn connect(&mut self) -> bool {
        self.peer = self.host.open().unwrap();
        self.host
            .submit(
                self.peer,
                Command::Connect {
                    bearer: "alice".into(),
                    client_id: "probe-client".into(),
                },
                self.now,
            )
            .unwrap();
        let replies = self.host.drain(self.peer).unwrap();
        let [Response::Attached { resumed }] = replies.as_slice() else {
            panic!("unexpected reconnect: {replies:?}");
        };
        *resumed
    }
}
