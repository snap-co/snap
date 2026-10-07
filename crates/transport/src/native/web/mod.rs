//! HTTP/WebSocket JSON IO. Transport owns operation execution and logical
//! connections. Socket tasks only decode, enqueue and write queued observations.
mod cookies;
mod http;
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
pub use cookies::Cookies;
use futures_util::{SinkExt, StreamExt};
pub use http::{HttpOperation, Parameters, WriteCookie, cookie, http_router, redirect};
use snap_transport::carrier::{Connection, Dispatch, Physical, Submission};
use std::{sync::Arc, time::Duration};

pub type ReadCookie = Arc<dyn Fn(&HeaderMap) -> Option<String> + Send + Sync>;

pub struct Service<D: Dispatch> {
    pub dispatch: D,
    pub origin: String,
    pub cookie: ReadCookie,
}

pub fn router<D: Dispatch>(service: Arc<Service<D>>) -> Router {
    Router::new()
        .route("/transport", get(upgrade::<D>))
        .with_state(service)
}

async fn upgrade<D: Dispatch>(
    State(service): State<Arc<Service<D>>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let authority = service
        .origin
        .strip_prefix("http://")
        .or_else(|| service.origin.strip_prefix("https://"))
        .unwrap_or("");
    if headers.get("host").and_then(|v| v.to_str().ok()) != Some(authority)
        || headers
            .get("origin")
            .is_some_and(|v| v.to_str().ok() != Some(&service.origin))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(bearer) = (service.cookie)(&headers).filter(|bearer| !bearer.is_empty()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    // Every upgrade identifies its caller before allocating a peer. There is no
    // anonymous WebSocket mode; credential acquisition belongs to HTTP.
    let channel = match service
        .dispatch
        .open(Some(bearer.clone()), 1024 * 64 * 1024)
        .await
    {
        Ok(channel) => Physical(channel),
        Err(snap_transport::Error::InvalidBearer | snap_transport::Error::IdentityRequired) => {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    // The physical guard is captured before upgrade, so failed upgrades also
    // disconnect without requiring a separate callback or an execution lock.
    ws.max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_upgrade(move |socket| connection::<D>(socket, bearer, channel))
}

async fn connection<D: Dispatch>(
    socket: WebSocket,
    cookie_bearer: String,
    channel: Physical<D::Connection>,
) {
    let (mut sink, mut stream) = socket.split();
    let mut flush = tokio::time::interval(Duration::from_millis(2));
    loop {
        tokio::select! {
            _ = flush.tick() => {},
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    let Ok(mut command) = serde_json::from_str(&text) else { break; };
                    if matches!(command, snap_transport::Command::Request { .. }) { break; }
                    // Connect names a logical lifetime, never a new authority.
                    if let snap_transport::Command::Connect { bearer, .. } = &mut command {
                        *bearer = cookie_bearer.clone();
                    }
                    if !matches!(channel.0.submit(command, text.len()), Ok(Submission::Queued)) { break; }
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {},
                _ => break,
            }
        }
        let retired = channel.0.retired();
        while let Some(frame) = channel.0.receive() {
            let Ok(text) = serde_json::to_string(&frame.response) else {
                return;
            };
            if !matches!(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    sink.send(Message::Text(text.into()))
                )
                .await,
                Ok(Ok(()))
            ) {
                return;
            }
            if frame.terminal {
                return;
            }
        }
        if retired {
            break;
        }
    }
}
