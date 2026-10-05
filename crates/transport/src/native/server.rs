use super::{Prepare, ReadCookie, driver, tcp, web};
use crate::{
    host::{Blocking, Participant},
    runtime::Loop,
};
use snap_store::{Backend, Host, Transaction};
use std::{future::IntoFuture, io, sync::Arc};

/// Bootstrap policy for the WebSocket upgrade. Applications own credential
/// decoding; Transport enforces origin checks and injects the selected bearer.
#[derive(Clone)]
pub struct WebSocket {
    pub origin: String,
    pub cookie: Option<ReadCookie>,
    pub require_cookie: bool,
}

/// Serialized server-side transactions without access to the execution gate,
/// dispatcher or physical peers. Clones share the same gate as socket operations.
pub struct Transactions<H: Loop> {
    shared: Arc<driver::Shared<H>>,
}
impl<H: Loop> Clone for Transactions<H> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}
impl<H: Loop + Host> Transactions<H> {
    /// May block behind accepted work and controller IO. Call from blocking
    /// execution, never reenter this handle from an operation or controller.
    pub fn run<T>(
        &self,
        name: &str,
        f: impl FnOnce(&mut Transaction<'_>) -> Result<T, snap_store::Error>,
    ) -> Result<T, snap_store::Error> {
        self.shared.host.lock().unwrap().transact(name, f)
    }
}
impl<H: Loop> From<Arc<driver::Shared<H>>> for Transactions<H> {
    fn from(shared: Arc<driver::Shared<H>>) -> Self {
        Self { shared }
    }
}

/// An application-selected Store and behavior with Transport-owned native
/// execution. Construction recovers controllers before routes can admit work.
pub struct Server<B: Backend, P: Participant<B>> {
    shared: Arc<driver::Shared<Blocking<B, P>>>,
}
impl<B: Backend + Send + 'static, P: Participant<B> + Send + 'static> Server<B, P> {
    pub async fn new(mut application: Blocking<B, P>) -> Result<Self, snap_store::Error> {
        let application = tokio::task::spawn_blocking(move || {
            application.recover()?;
            Ok::<_, snap_store::Error>(application)
        })
        .await
        .map_err(|_| snap_store::Error::Unavailable)??;
        Ok(Self {
            shared: driver::Shared::new(application),
        })
    }

    pub fn transactions(&self) -> Transactions<Blocking<B, P>> {
        Transactions {
            shared: self.shared.clone(),
        }
    }

    /// Mount into the application's router. Creating routes starts no tasks;
    /// `run` must be driven for admitted operations to execute.
    pub fn websocket(&self, options: WebSocket) -> axum::Router {
        web::router(Arc::new(web::Service {
            dispatch: driver::Dispatcher::web(self.shared.clone()),
            origin: options.origin,
            cookie: options.cookie,
            require_cookie: options.require_cookie,
        }))
    }

    /// Mount connectionless operations with the same origin and credential policy
    /// as WebSocket traffic. Cookie changes are published only after commit.
    pub fn http(&self, options: WebSocket, operations: Vec<web::HttpOperation>) -> axum::Router {
        web::http_router(
            Arc::new(web::Service {
                dispatch: driver::Dispatcher::web(self.shared.clone()),
                origin: options.origin,
                cookie: options.cookie,
                require_cookie: options.require_cookie,
            }),
            operations,
        )
    }

    /// Drive HTTP/WebSocket and execution without opening a TCP listener.
    pub async fn run_http(self, http: impl IntoFuture<Output = io::Result<()>>) -> io::Result<()> {
        tokio::select! {
            result = http.into_future() => result,
            _ = driver::dispatch(self.shared) => Err(io::Error::other("Transport execution stopped")),
        }
    }

    /// Drive the app-owned HTTP server, TCP listener and execution pump together.
    /// HTTP owns graceful shutdown policy. Returning from HTTP or a TCP listener
    /// error stops the other futures, as in the existing native host assembly.
    /// Physical loss never cancels accepted work while this server is running.
    pub async fn run(
        self,
        listener: tokio::net::TcpListener,
        tls: tcp::tls::ServerTls,
        prepare: Option<Prepare>,
        http: impl IntoFuture<Output = io::Result<()>>,
    ) -> io::Result<()> {
        let dispatch = driver::Dispatcher::tcp(self.shared.clone(), prepare);
        tokio::select! {
            result = http.into_future() => result,
            result = tcp::serve(listener, dispatch, tls) => result,
            _ = driver::dispatch(self.shared) => Err(io::Error::other("Transport execution stopped")),
        }
    }
}
