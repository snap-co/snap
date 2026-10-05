//! Native paired setup: production TCP/TLS and dispatch, file-backed SQLite,
//! and the portable cartridge SDK journey. No transport observations are faked.
#[path = "host.rs"]
mod host;
#[path = "../../../crates/transport/tests/support/mod.rs"]
mod tls_support;

use snap_platform_tests::{cartridge, journey};
use snap_store_sqlite::Sqlite;
use snap_transport::host::Blocking as Host;
use snap_transport::native::driver::{Dispatcher, Shared};
use snap_transport::{Channel, Command, Error, Response, client::Client};
use std::{io, net::SocketAddr, path::Path, sync::Arc, time::Duration};
use tokio::task::JoinHandle;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Host-level adaptation to Channel. Framing and verified TLS remain in the
// production driver. A physical failure fences this channel; it never retries.
struct TcpChannel {
    driver: snap_transport::native::TcpClient,
    usable: bool,
}
impl Channel for TcpChannel {
    async fn send(&mut self, command: Command) -> std::result::Result<(), Error> {
        if !self.usable {
            return Err(Error::Unavailable);
        }
        self.driver.send(&command).await.map_err(|_| {
            self.usable = false;
            Error::Unavailable
        })
    }
    async fn receive(&mut self) -> std::result::Result<Option<Response>, Error> {
        if !self.usable {
            return Err(Error::Unavailable);
        }
        self.driver
            .receive()
            .await
            .map(|(response, _)| Some(response))
            .map_err(|_| {
                self.usable = false;
                Error::Unavailable
            })
    }
}

struct Server {
    shared: Arc<Shared<Host<Sqlite>>>,
    serving: JoinHandle<io::Result<()>>,
    dispatch: JoinHandle<()>,
    address: SocketAddr,
    client_tls: snap_transport::native::tls::ClientTls,
    _pki: tempfile::TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.serving.abort();
        self.dispatch.abort();
    }
}
impl Server {
    async fn start(database: &Path) -> Result<Self> {
        // SQLite may remove or modify companions even when its main file is new.
        // Reject dangling links too. This local runner assumes its directory is
        // not concurrently modified; these checks are not a filesystem sandbox.
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut companion = database.as_os_str().to_os_string();
            companion.push(suffix);
            match std::fs::symlink_metadata(&companion) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "refusing existing SQLite companion {}",
                            Path::new(&companion).display()
                        ),
                    )
                    .into());
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        // Reserve a new main path; never migrate or reset an existing database.
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(database)?,
        );
        snap_store_sqlite::migrate(database, &host::migrations())?;
        let store = Sqlite::open(database)?;
        let pki = tempfile::tempdir()?;
        let (server_tls, client_tls) = tls_support::pki(pki.path(), false);
        // Each mount has a distinct boot namespace, supplied by native assembly.
        let boot = pki.path().to_string_lossy().into_owned();
        let shared = Shared::new(host::mount(store, Default::default(), boot)?);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let serving = tokio::spawn(snap_transport::native::tcp::serve(
            listener,
            Dispatcher::tcp(shared.clone(), None),
            server_tls,
        ));
        let dispatch = tokio::spawn(snap_transport::native::driver::dispatch(shared.clone()));
        Ok(Self {
            shared,
            serving,
            dispatch,
            address,
            client_tls,
            _pki: pki,
        })
    }

    async fn stop(mut self) -> Result<()> {
        self.serving.abort();
        self.dispatch.abort();
        let serving = (&mut self.serving).await;
        let dispatch = (&mut self.dispatch).await;
        // Carrier workers and any already-running blocking dispatch must release
        // their host ownership before the SQLite exclusive lock can be reopened.
        tokio::time::timeout(Duration::from_secs(5), async {
            while Arc::strong_count(&self.shared) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        match serving {
            Ok(result) => result?,
            Err(error) if !error.is_cancelled() => return Err(error.into()),
            _ => {}
        }
        if let Err(error) = dispatch
            && !error.is_cancelled()
        {
            return Err(error.into());
        }
        Ok(())
    }
}

/// A deadline bounds connection and journey execution, not synchronous startup
/// or database reopening. Teardown has a separate ownership-release deadline.
/// After stopping the server, reopen SQLite and check persisted rows against the
/// independent model, not resident state. This is orderly reopen, not crash proof.
pub async fn run(
    database: &Path,
    mut report: impl FnMut(journey::Observation<'_>),
) -> Result<[i64; 2]> {
    let server = Server::start(database).await?;
    let execution = tokio::time::timeout(Duration::from_secs(30), async {
        let driver = snap_transport::native::TcpClient::open(
            &server.address.to_string(),
            &server.client_tls,
        )
        .await?;
        let mut client = Client::new(TcpChannel {
            driver,
            usable: true,
        });
        let resumed = client
            .connect("alice", "plumbing-client")
            .await
            .map_err(|error| io::Error::other(format!("connect: {error:?}")))?;
        if resumed {
            return Err(io::Error::other("fresh connection unexpectedly resumed").into());
        }
        let value = journey::run(&mut client, &mut report)
            .await
            .map_err(|error| io::Error::other(format!("cartridge: {error:?}")))?;
        Ok::<_, Box<dyn std::error::Error>>(value)
    })
    .await;
    server.stop().await?;
    let expected = execution??;
    let mut store = Sqlite::open(database)?;
    for table in cartridge::TABLES {
        store.load(table)?;
    }
    let rows = store.inspect("probe.persisted", cartridge::read)?;
    if rows != [expected, expected] {
        return Err(io::Error::other(format!(
            "reopened rows {rows:?}, expected [{expected}, {expected}]"
        ))
        .into());
    }
    Ok(rows)
}
