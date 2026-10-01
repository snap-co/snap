//! Host debugger protocol, independent of application attachments and the executor
//! queue. Commands run synchronously at host boundaries; socket writes never hold
//! the host lock. Slow observers skip intermediate reports and receive the latest
//! full state. Reconnecting observes current state; commands are never replayed.
use super::{Host, local};
use crate::development::Control;
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use snap_transport::execution::Program;
use snap_transport::{Value, json, server::Authority};
use std::time::Duration;

pub(super) struct Report {
    state: Value,
    revision: u64,
    encoded: String,
}
impl Report {
    pub(super) fn new(state: Value) -> Self {
        let mut report = Self {
            state,
            revision: 0,
            encoded: String::new(),
        };
        report.encode();
        report
    }
    fn encode(&mut self) {
        self.encoded =
            json!({"type": "state", "revision": self.revision.to_string(), "state": self.state})
                .to_string();
    }
    pub(super) fn update(&mut self, state: Value) -> bool {
        if self.state == state {
            return false;
        }
        self.state = state;
        self.revision += 1;
        self.encode();
        true
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    control: Value,
}

pub(super) async fn upgrade<P: Program + Send + 'static, R: Authority + Send + 'static>(
    State(shared): State<Host<P, R>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !local(&headers, &shared.authority) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // Debuggers allocate no application peer and closing them changes no state.
    ws.max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_upgrade(move |socket| connection(socket, shared))
}

async fn connection<P: Program + Send + 'static, R: Authority + Send + 'static>(
    socket: WebSocket,
    shared: Host<P, R>,
) {
    let mut updates = shared.updates.subscribe();
    let (mut sink, mut stream) = socket.split();
    let initial = updates.borrow_and_update().encoded.clone();
    if !send(&mut sink, initial).await {
        return;
    }
    loop {
        let outgoing = tokio::select! {
            changed = updates.changed() => {
                if changed.is_err() { break; }
                updates.borrow_and_update().encoded.clone()
            }
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    let Ok(request) = serde_json::from_str::<Request>(&text) else { break; };
                    if request.id.is_empty() || request.id.len() > 128 { break; }
                    let result = match serde_json::from_value::<Control>(request.control) {
                        Err(error) => Err(error.to_string()),
                        Ok(control) => {
                            let shared = shared.clone();
                            tokio::task::spawn_blocking(move || shared.change(|host| host.control(control, shared.clock.elapsed().as_millis() as u64)))
                                .await.unwrap_or_else(|_| Err("host control unavailable".into()))
                        }
                    };
                    match result {
                        Ok(result) => json!({"type": "result", "id": request.id, "result": result}).to_string(),
                        Err(error) => json!({"type": "result", "id": request.id, "error": error}).to_string(),
                    }
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                _ => break,
            }
        };
        if !send(&mut sink, outgoing).await {
            break;
        }
    }
}

async fn send(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    encoded: String,
) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(5),
            sink.send(Message::Text(encoded.into()))
        )
        .await,
        Ok(Ok(()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_observer_resynchronizes_without_an_unbounded_queue() {
        let (updates, mut observer) = tokio::sync::watch::channel(Report::new(json!({"value": 0})));
        assert_eq!(observer.borrow_and_update().revision, 0);
        for value in 1..=1000 {
            updates.send_if_modified(|report| report.update(json!({"value": value})));
        }
        let latest: Value = serde_json::from_str(&observer.borrow_and_update().encoded).unwrap();
        assert_eq!(latest["revision"], "1000");
        assert_eq!(latest["state"]["value"], 1000);
        updates.send_if_modified(|report| report.update(json!({"value": 1000})));
        assert!(!observer.has_changed().unwrap());
    }
}
