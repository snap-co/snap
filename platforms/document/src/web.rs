//! Loopback WebSocket carrier. Readers and the global dispatcher are independent;
//! a slow socket never holds Store's mutation gate.
use crate::Host;
use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use snap_store::Backend;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tower_http::services::{ServeDir, ServeFile};

pub struct Shared<B: Backend> {
    pub host: Mutex<Host<B>>,
    clock: Instant,
    origin: String,
    cookie: Option<ReadCookie>,
    require_cookie: bool,
}

pub type ReadCookie = Arc<dyn Fn(&HeaderMap) -> Option<String> + Send + Sync>;

pub type WriteCookie = Arc<dyn Fn(Option<&str>) -> String + Send + Sync>;

/// Public HTTP projection of a registered pre-connection operation. The path is
/// derived from its operation name. Cookie issuance is a carrier concern, never
/// a second authentication handler. Only committed outcomes reach this adapter.
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
    let mut router = Router::new();
    for operation in operations {
        let path = format!("/{}", operation.name.replace('.', "/"));
        let shared = shared.clone();
        let write_cookie = write_cookie.clone();
        let method = operation.method.clone();
        let handler = move |headers: HeaderMap, body: axum::body::Bytes| {
            let shared = shared.clone();
            let write_cookie = write_cookie.clone();
            let method = method.clone();
            async move {
                let id = headers
                    .get("x-snap-operation-id")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|id| *id > 0)
                    .unwrap_or(1);
                let mut outcome = if method == Method::POST
                    && headers.get("origin").and_then(|v| v.to_str().ok()) != Some(&shared.origin)
                {
                    Err(snap_transport::Error::Application(
                        serde_json::json!({"code":"Forbidden"}),
                    ))
                } else {
                    let input = if method == Method::GET {
                        if body.is_empty() {
                            Ok(serde_json::Value::Null)
                        } else {
                            Err(snap_transport::Error::InvalidInput)
                        }
                    } else if headers
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.split(';').next())
                        != Some("application/json")
                    {
                        Err(snap_transport::Error::InvalidInput)
                    } else {
                        serde_json::from_slice(&body)
                            .map_err(|_| snap_transport::Error::InvalidInput)
                    };
                    match input {
                        Err(error) => Err(error),
                        Ok(input) => {
                            let bearer = match operation.session {
                                SessionProjection::Issue => None,
                                SessionProjection::Fetch => {
                                    shared.cookie.as_ref().and_then(|read| read(&headers))
                                }
                            };
                            let shared = shared.clone();
                            tokio::task::spawn_blocking(move || {
                                shared.host.lock().unwrap().http_request(
                                    snap_transport::Invocation {
                                        id,
                                        operation: operation.name.into(),
                                        input,
                                    },
                                    bearer,
                                )
                            })
                            .await
                            .expect("HTTP transport dispatcher panicked")
                        }
                    }
                };
                let mut cookie = None;
                match operation.session {
                    SessionProjection::Issue => {
                        if let Ok(value) = &mut outcome {
                            let bearer = value
                                .as_object_mut()
                                .and_then(|value| value.remove("bearer"));
                            if let Some(bearer) = bearer.as_ref().and_then(|value| value.as_str()) {
                                cookie = Some(write_cookie(Some(bearer)));
                            } else {
                                outcome = Err(snap_transport::Error::Protocol);
                            }
                        }
                    }
                    SessionProjection::Fetch => {
                        if matches!(outcome, Err(snap_transport::Error::InvalidBearer)) {
                            outcome = Ok(serde_json::Value::Null);
                        }
                        if outcome.as_ref().is_ok_and(|value| value.is_null()) {
                            cookie = Some(write_cookie(None));
                        }
                    }
                }
                let status = match &outcome {
                    Ok(_) => StatusCode::OK,
                    Err(snap_transport::Error::InvalidInput) => StatusCode::BAD_REQUEST,
                    Err(
                        snap_transport::Error::InvalidBearer
                        | snap_transport::Error::IdentityRequired,
                    ) => StatusCode::UNAUTHORIZED,
                    Err(snap_transport::Error::Application(value))
                        if value["code"] == "Forbidden" =>
                    {
                        StatusCode::FORBIDDEN
                    }
                    Err(snap_transport::Error::UnknownOperation) => StatusCode::NOT_FOUND,
                    Err(snap_transport::Error::Application(value))
                        if value["code"] == "Conflict" =>
                    {
                        StatusCode::CONFLICT
                    }
                    Err(_) => StatusCode::SERVICE_UNAVAILABLE,
                };
                let mut response = (
                    status,
                    [("cache-control", "no-store")],
                    Json(snap_transport::Event::Completed { id, outcome }),
                )
                    .into_response();
                if let Some(cookie) = cookie {
                    response
                        .headers_mut()
                        .insert("set-cookie", cookie.parse().expect("session cookie"));
                }
                response
            }
        };
        router = router.route(
            &path,
            if operation.method == Method::GET {
                get(handler)
            } else {
                axum::routing::post(handler)
            },
        );
    }
    router
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

    /// Browser-only carrier: validate the cookie before WebSocket upgrade and
    /// use that authority for Connect regardless of client-supplied credentials.
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

pub fn router<B: Backend + Send + 'static>(shared: Arc<Shared<B>>) -> Router {
    Router::new()
        .route("/transport", get(upgrade::<B>))
        .with_state(shared)
}

async fn upgrade<B: Backend + Send + 'static>(
    State(shared): State<Arc<Shared<B>>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let authority = shared
        .origin
        .strip_prefix("http://")
        .or_else(|| shared.origin.strip_prefix("https://"))
        .unwrap_or("");
    if headers.get("host").and_then(|v| v.to_str().ok()) != Some(authority)
        || headers
            .get("origin")
            .is_some_and(|v| v.to_str().ok() != Some(&shared.origin))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let opening = shared.clone();
    let bearer = shared.cookie.as_ref().and_then(|read| read(&headers));
    if shared.require_cookie && bearer.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let credential = bearer.clone();
    let opened = tokio::task::spawn_blocking(move || {
        let mut host = opening.host.lock().unwrap();
        if opening.require_cookie
            && let Some(bearer) = credential
        {
            host.authorize_upgrade(&bearer)?;
        }
        host.open()
            .and_then(|peer| Ok((peer, host.output(peer)?, host.carrier_control(peer)?)))
    })
    .await
    .expect("document carrier opening panicked");
    let (peer, output, control) = match opened {
        Ok(value) => value,
        Err(snap_transport::Error::InvalidBearer | snap_transport::Error::IdentityRequired) => {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let failed = shared.clone();
    let failed_control = control.clone();
    ws.max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_failed_upgrade(move |_| failed_control.detach(failed.now()))
        .on_upgrade(move |socket| connection(socket, shared, peer, bearer, output, control))
}

async fn connection<B: Backend + Send + 'static>(
    socket: WebSocket,
    shared: Arc<Shared<B>>,
    peer: u64,
    cookie_bearer: Option<String>,
    output: crate::Output,
    control: crate::CarrierControl,
) {
    let (mut sink, mut stream) = socket.split();
    let mut flush = tokio::time::interval(Duration::from_millis(2));
    let mut pending = std::collections::VecDeque::new();
    loop {
        let mut error = None;
        tokio::select! {
            _ = flush.tick() => {},
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    let Ok(mut command) = serde_json::from_str(&text) else { break; };
                    if shared.require_cookie && matches!(command, snap_transport::Command::Request { .. }) { break; }
                    if matches!(command, snap_transport::Command::Close | snap_transport::Command::Disconnect) {
                        if matches!(command, snap_transport::Command::Close) { control.close(shared.now()); }
                        else { control.detach(shared.now()); }
                        break;
                    }
                    if let snap_transport::Command::Connect { bearer, .. } = &mut command
                        && (shared.require_cookie || bearer.is_empty())
                        && let Some(cookie) = &cookie_bearer {
                        *bearer = cookie.clone();
                    }
                    if pending.len() >= 1024 { break; }
                    pending.push_back(command);
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {},
                _ => break,
            }
        }
        if let Ok(mut host) = shared.host.try_lock() {
            while let Some(command) = pending.pop_front() {
                if let Err(failed) = host.submit(peer, command, shared.now()) {
                    error = Some(failed);
                    break;
                }
            }
        }
        let mut failed = false;
        loop {
            let response = if let Some(error) = error.take() {
                snap_transport::Response::Failed(error)
            } else {
                match output.pop_front() {
                    Some(response) => response,
                    None => break,
                }
            };
            let Ok(text) = serde_json::to_string(&response) else {
                failed = true;
                break;
            };
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                sink.send(Message::Text(text.into())),
            )
            .await;
            if !matches!(result, Ok(Ok(()))) {
                failed = true;
                break;
            }
        }
        if failed || shared.host.try_lock().is_ok_and(|host| host.retired(peer)) {
            break;
        }
    }
    // Drop both halves without waiting for synchronous IO or a socket close handshake.
    control.detach(shared.now());
    drop(stream);
    drop(sink);
}

/// Drive one application-wide FIFO. Tests can instead call Host::step explicitly
/// to exercise ACK/completion separation without timers or sleeps.
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
