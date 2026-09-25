//! Local browser host. HTTP controls and WebSockets drive the same Development
//! instance. The listener must be loopback: these controls expose resident data.
use crate::development::{Control, Development};
use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use snap_execution::Program;
use snap_transport::{json, server::Authority};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tower_http::services::{ServeDir, ServeFile};

struct Shared<P: Program, R: Authority> {
    host: Mutex<Development<P, R>>,
    clock: Instant,
    authority: String,
}
type Host<P, R> = Arc<Shared<P, R>>;

// Reject cross-origin browser control requests and DNS-rebinding Host headers.
fn local(headers: &HeaderMap, authority: &str) -> bool {
    headers.get("host").and_then(|h| h.to_str().ok()) == Some(authority)
        && headers
            .get("origin")
            .is_none_or(|origin| origin.to_str().ok() == Some(&format!("http://{authority}")))
}
async fn inspect<P: Program, R: Authority>(
    State(shared): State<Host<P, R>>,
    headers: HeaderMap,
) -> Response {
    if !local(&headers, &shared.authority) {
        return StatusCode::FORBIDDEN.into_response();
    }
    Json(shared.host.lock().unwrap().inspect()).into_response()
}
async fn control<P: Program, R: Authority>(
    State(shared): State<Host<P, R>>,
    headers: HeaderMap,
    Json(control): Json<Control>,
) -> Response {
    if !local(&headers, &shared.authority) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match shared
        .host
        .lock()
        .unwrap()
        .control(control, shared.clock.elapsed().as_millis() as u64)
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => (StatusCode::CONFLICT, Json(json!({"error": error}))).into_response(),
    }
}
async fn upgrade<P: Program + Send + 'static, R: Authority + Send + 'static>(
    State(shared): State<Host<P, R>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !local(&headers, &shared.authority) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let peer = match shared.host.lock().unwrap().open() {
        Ok(id) => id,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let failed = shared.clone();
    ws.max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_failed_upgrade(move |_| {
            failed
                .host
                .lock()
                .unwrap()
                .lost(peer, failed.clock.elapsed().as_millis() as u64)
        })
        .on_upgrade(move |socket| connection(socket, shared, peer))
}
async fn connection<P: Program + Send + 'static, R: Authority + Send + 'static>(
    socket: WebSocket,
    shared: Host<P, R>,
    peer: u64,
) {
    let (mut sink, mut stream) = socket.split();
    let mut flush = tokio::time::interval(Duration::from_millis(10));
    loop {
        tokio::select! {
            _ = flush.tick() => {},
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    let Ok(command) = serde_json::from_str(&text) else { break; };
                    if shared.host.lock().unwrap().send(peer, command, shared.clock.elapsed().as_millis() as u64).is_err() { break; }
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {},
                _ => break,
            }
        }
        let responses = shared.host.lock().unwrap().drain(peer);
        let Ok(responses) = responses else {
            break;
        };
        let mut failed = false;
        for response in responses {
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                sink.send(Message::Text(
                    serde_json::to_string(&response).unwrap().into(),
                )),
            )
            .await;
            if !matches!(result, Ok(Ok(()))) {
                failed = true;
                break;
            }
        }
        if failed {
            break;
        }
    }
    shared
        .host
        .lock()
        .unwrap()
        .lost(peer, shared.clock.elapsed().as_millis() as u64);
}
pub async fn serve<P: Program + Send + 'static, R: Authority + Send + 'static>(
    listener: tokio::net::TcpListener,
    host: Development<P, R>,
    assets: String,
) -> std::io::Result<()> {
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err(std::io::Error::other(
            "development controls require a loopback listener",
        ));
    }
    let shared = Arc::new(Shared {
        host: Mutex::new(host),
        clock: Instant::now(),
        authority: address.to_string(),
    });
    let app = Router::new()
        .route("/transport", get(upgrade::<P, R>))
        .route("/__dev", get(inspect::<P, R>).post(control::<P, R>))
        .fallback_service(
            ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
        )
        .with_state(shared.clone());
    let sweep = async {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            interval.tick().await;
            shared
                .host
                .lock()
                .unwrap()
                .tick(shared.clock.elapsed().as_millis() as u64);
        }
    };
    tokio::select! { result = axum::serve(listener, app) => result, _ = sweep => unreachable!() }
}
