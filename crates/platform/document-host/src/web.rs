//! Document host composition for the independent HTTP/WebSocket carrier.
use crate::{Host, carrier::Dispatcher};
use axum::{
    Router,
    http::{Method, StatusCode},
};
use snap_store::Backend;
pub use snap_transport_ws::ReadCookie;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tower_http::services::{ServeDir, ServeFile};
pub type WriteCookie = Arc<dyn Fn(Option<&str>) -> String + Send + Sync>;

pub struct Shared<B: Backend> {
    pub host: Mutex<Host<B>>,
    clock: Instant,
    origin: String,
    cookie: Option<ReadCookie>,
    require_cookie: bool,
}

/// Host-selected session projection. Identity policy stays in composition; the
/// HTTP driver only decodes requests and projects the committed response.
pub struct HttpOperation {
    pub name: &'static str,
    pub method: Method,
    pub session: SessionProjection,
}
#[derive(Clone, Copy)]
pub enum SessionProjection {
    Issue,
    Fetch,
}

pub fn http_router<B: Backend + Send + 'static>(
    shared: Arc<Shared<B>>,
    operations: Vec<HttpOperation>,
    write_cookie: WriteCookie,
) -> Router {
    let operations = operations
        .into_iter()
        .map(|operation| {
            let write_cookie = write_cookie.clone();
            snap_transport_ws::HttpOperation {
                name: operation.name,
                method: operation.method,
                read_cookie: matches!(operation.session, SessionProjection::Fetch),
                project: Arc::new(move |outcome| {
                    let mut cookie = None;
                    match operation.session {
                        SessionProjection::Issue => {
                            if let Ok(value) = outcome {
                                let bearer = value
                                    .as_object_mut()
                                    .and_then(|value| value.remove("bearer"));
                                if let Some(bearer) =
                                    bearer.as_ref().and_then(|value| value.as_str())
                                {
                                    cookie = Some(write_cookie(Some(bearer)));
                                } else {
                                    *outcome = Err(snap_transport::Error::Protocol);
                                }
                            }
                        }
                        SessionProjection::Fetch => {
                            if matches!(outcome, Err(snap_transport::Error::InvalidBearer)) {
                                *outcome = Ok(serde_json::Value::Null);
                            }
                            if outcome.as_ref().is_ok_and(|value| value.is_null()) {
                                cookie = Some(write_cookie(None));
                            }
                        }
                    }
                    let status = match outcome {
                        Err(snap_transport::Error::Application(value))
                            if value["code"] == "Forbidden" =>
                        {
                            Some(StatusCode::FORBIDDEN)
                        }
                        Err(snap_transport::Error::Application(value))
                            if value["code"] == "Conflict" =>
                        {
                            Some(StatusCode::CONFLICT)
                        }
                        _ => None,
                    };
                    snap_transport_ws::HttpProjection { cookie, status }
                }),
            }
        })
        .collect();
    snap_transport_ws::http_router(service(shared), operations)
}

impl<B: Backend> Shared<B> {
    pub fn new(host: Host<B>, origin: String) -> Arc<Self> {
        Arc::new(Self {
            host: Mutex::new(host),
            clock: Instant::now(),
            origin,
            cookie: None,
            require_cookie: false,
        })
    }
    /// Mixed browser/agent carrier. An empty Connect bearer uses the browser
    /// cookie; explicit credentials still reach the configured authority.
    pub fn with_cookie(host: Host<B>, origin: String, cookie: ReadCookie) -> Arc<Self> {
        Arc::new(Self {
            host: Mutex::new(host),
            clock: Instant::now(),
            origin,
            cookie: Some(cookie),
            require_cookie: false,
        })
    }
    /// Browser-only carrier. Validate cookie authority before upgrade, then use
    /// that cookie for Connect regardless of client-supplied credentials.
    pub fn with_required_cookie(host: Host<B>, origin: String, cookie: ReadCookie) -> Arc<Self> {
        Arc::new(Self {
            host: Mutex::new(host),
            clock: Instant::now(),
            origin,
            cookie: Some(cookie),
            require_cookie: true,
        })
    }
    pub(crate) fn now(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }
}

fn service<B: Backend + Send + 'static>(
    shared: Arc<Shared<B>>,
) -> Arc<snap_transport_ws::Service<Dispatcher<B>>> {
    Arc::new(snap_transport_ws::Service {
        dispatch: Dispatcher::web(shared.clone()),
        origin: shared.origin.clone(),
        cookie: shared.cookie.clone(),
        require_cookie: shared.require_cookie,
    })
}
pub fn router<B: Backend + Send + 'static>(shared: Arc<Shared<B>>) -> Router {
    snap_transport_ws::router(service(shared))
}

/// Drive the application FIFO independently of socket tasks. Tests can instead
/// step Host explicitly to observe admission separately from execution.
pub async fn dispatch<B: Backend + Send + 'static>(shared: Arc<Shared<B>>) {
    let mut interval = tokio::time::interval(Duration::from_millis(5));
    loop {
        interval.tick().await;
        let shared = shared.clone();
        tokio::task::spawn_blocking(move || {
            let mut host = shared.host.lock().unwrap();
            host.tick(shared.now());
            host.step();
        })
        .await
        .expect("document dispatcher panicked");
    }
}
pub async fn serve<B: Backend + Send + 'static>(
    listener: tokio::net::TcpListener,
    host: Host<B>,
    assets: String,
) -> std::io::Result<()> {
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err(std::io::Error::other(
            "document development host requires loopback",
        ));
    }
    let shared = Shared::new(host, format!("http://{address}"));
    let app = router(shared.clone()).fallback_service(
        ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
    );
    tokio::select! { result = axum::serve(listener, app) => result, _ = dispatch(shared) => unreachable!() }
}
