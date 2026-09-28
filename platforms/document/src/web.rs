//! Loopback WebSocket carrier. Readers and the global dispatcher are independent;
//! a slow socket never holds Store's mutation gate.
use crate::Host;
use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
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
}

pub type ReadCookie = Arc<dyn Fn(&HeaderMap) -> Option<String> + Send + Sync>;

impl<B: Backend> Shared<B> {
    pub fn new(host: Host<B>, origin: String) -> Arc<Self> {
        Arc::new(Self {
            host: Mutex::new(host),
            clock: Instant::now(),
            origin,
            cookie: None,
        })
    }

    pub fn with_cookie(host: Host<B>, origin: String, cookie: ReadCookie) -> Arc<Self> {
        Arc::new(Self {
            host: Mutex::new(host),
            clock: Instant::now(),
            origin,
            cookie: Some(cookie),
        })
    }

    fn now(&self) -> u64 {
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
    let opened = tokio::task::spawn_blocking(move || {
        let mut host = opening.host.lock().unwrap();
        host.open()
            .and_then(|peer| Ok((peer, host.output(peer)?, host.carrier_control(peer)?)))
    })
    .await
    .expect("document carrier opening panicked");
    let (peer, output, control) = match opened {
        Ok(value) => value,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let failed = shared.clone();
    let failed_control = control.clone();
    let bearer = shared.cookie.as_ref().and_then(|read| read(&headers));
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
                    if matches!(command, snap_transport::Command::Close | snap_transport::Command::Disconnect) {
                        if matches!(command, snap_transport::Command::Close) { control.close(shared.now()); }
                        else { control.detach(shared.now()); }
                        break;
                    }
                    if let snap_transport::Command::Connect { bearer, .. } = &mut command
                        && bearer.is_empty()
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
