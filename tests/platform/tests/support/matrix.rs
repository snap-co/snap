//! Host configuration is the variable. The portable client journey and its
//! independent expectations are shared across Store × carrier combinations.
#[path = "../../support/host.rs"]
mod assembly;
#[path = "../../../../crates/transport/tests/support/mod.rs"]
mod tls_support;

use futures_util::{SinkExt, StreamExt};
use snap_platform_tests::{
    configuration::{Carrier, Storage},
    journey,
    memory::Memory,
};
use snap_store::{Backend, Catalog, Store};
use snap_store_sqlite::Sqlite;
use snap_transport::native::driver::{Dispatcher, Shared};
use snap_transport::{Channel, Command, Error, Response, client::Client, host::Blocking};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

struct Tasks(Vec<tokio::task::JoinHandle<()>>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}
impl Tasks {
    async fn stop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
        for task in self.0.drain(..) {
            if let Err(error) = task.await {
                assert!(error.is_cancelled(), "host task failed: {error}");
            }
        }
    }
}

struct Controlled<B: Backend> {
    host: Blocking<B>,
    peer: u64,
    pending: std::collections::VecDeque<Response>,
}
impl<B: Backend> Channel for Controlled<B> {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        self.host.submit(self.peer, command, 0)
    }
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        // Drive actual production execution, not manufactured acceptance/results.
        if self.pending.is_empty() {
            self.pending.extend(self.host.drain(self.peer)?);
        }
        if self.pending.is_empty() {
            self.host.step();
            self.pending.extend(self.host.drain(self.peer)?);
        }
        Ok(self.pending.pop_front())
    }
}

struct Tcp(snap_transport::native::TcpClient);
impl Channel for Tcp {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        self.0.send(&command).await.map_err(|_| Error::Unavailable)
    }
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        self.0
            .receive()
            .await
            .map(|(response, _)| Some(response))
            .map_err(|_| Error::Unavailable)
    }
}

struct WebSocket(
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
);
impl Channel for WebSocket {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        self.0
            .send(Message::Text(
                serde_json::to_string(&command).unwrap().into(),
            ))
            .await
            .map_err(|_| Error::Unavailable)
    }
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        loop {
            let Some(message) = self.0.next().await else {
                return Err(Error::Unavailable);
            };
            match message.map_err(|_| Error::Unavailable)? {
                Message::Text(text) => {
                    return serde_json::from_str(&text)
                        .map(Some)
                        .map_err(|_| Error::Unavailable);
                }
                Message::Close(_) => return Err(Error::Unavailable),
                _ => {}
            }
        }
    }
}

async fn check(channel: impl Channel) {
    let mut client = Client::new(channel);
    assert!(!client.connect("alice", "matrix-client").await.unwrap());
    assert_eq!(journey::run(&mut client, |_| {}).await.unwrap(), 5);
}

async fn configured<B: Backend + Send + 'static>(store: Store<B>, carrier: Carrier) {
    let mut host = assembly::mount(store, Default::default(), "matrix-host".into()).unwrap();
    if carrier == Carrier::Controlled {
        let peer = host.open().unwrap();
        check(Controlled {
            host,
            peer,
            pending: Default::default(),
        })
        .await;
        return;
    }
    let shared = Shared::new(host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let dispatch = shared.clone();
    let mut tasks = Tasks(vec![tokio::spawn(async move {
        snap_transport::native::driver::dispatch(dispatch).await;
    })]);
    match carrier {
        Carrier::Tcp => {
            let pki = tempfile::tempdir().unwrap();
            let (server_tls, client_tls) = tls_support::pki(pki.path(), false);
            let dispatch = Dispatcher::tcp(shared.clone(), None);
            tasks.0.push(tokio::spawn(async move {
                snap_transport::native::tcp::serve(listener, dispatch, server_tls)
                    .await
                    .unwrap();
            }));
            check(Tcp(snap_transport::native::TcpClient::open(
                &address.to_string(),
                &client_tls,
            )
            .await
            .unwrap()))
            .await;
        }
        Carrier::WebSocket => {
            let app = snap_transport::native::web::router(Arc::new(
                snap_transport::native::web::Service {
                    dispatch: Dispatcher::web(shared.clone()),
                    origin: format!("http://{address}"),
                    cookie: Arc::new(|headers| {
                        headers.get("cookie")?.to_str().ok().map(str::to_owned)
                    }),
                },
            ));
            tasks.0.push(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            }));
            let mut request = format!("ws://{address}/transport")
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("cookie", "alice".parse().unwrap());
            let (socket, _) = connect_async(request).await.unwrap();
            check(WebSocket(socket)).await;
        }
        Carrier::Controlled => unreachable!(),
    }
    tasks.stop().await;
    // Blocking dispatch and carrier workers may finish after task cancellation.
    // Release all host owners before removing a file-backed Store's directory.
    tokio::time::timeout(Duration::from_secs(5), async {
        while Arc::strong_count(&shared) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

pub async fn run(storage: Storage, carrier: Carrier) {
    tokio::time::timeout(Duration::from_secs(30), async {
        match storage {
            Storage::Memory => {
                let catalog = assembly::migrations()
                    .iter()
                    .try_fold(Catalog::default(), |catalog, migration| {
                        migration.apply(&catalog)
                    })
                    .unwrap();
                configured(
                    Store::new(catalog.clone(), Memory::new(catalog).unwrap()).unwrap(),
                    carrier,
                )
                .await;
            }
            Storage::SqliteMemory => {
                configured(Sqlite::memory(&assembly::migrations()).unwrap(), carrier).await
            }
            Storage::SqliteFile => {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("matrix.sqlite");
                snap_store_sqlite::migrate(&path, &assembly::migrations()).unwrap();
                configured(Sqlite::open(&path).unwrap(), carrier).await;
            }
        }
    })
    .await
    .expect("host configuration timed out");
}
