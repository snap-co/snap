//! Physical socket ownership. This slice admits read-only Identity Messages.
//! A fresh physical attachment gets a fresh logical epoch; no mutation replay or
//! detached result retention is promised. Reads can be explicitly issued again.
use crate::{Host, execute, passport, problem};
use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use snap_protocol::{Completion, Error, Invocation, json};
use std::{collections::HashMap, time::Duration};

pub(super) async fn upgrade(
    State(host): State<Host>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(passport) = &host.passport else {
        return problem(StatusCode::NOT_FOUND, "Not found");
    };
    if headers.get("origin").and_then(|v| v.to_str().ok()) != Some(passport.origin.as_str()) {
        return problem(StatusCode::FORBIDDEN, "Origin not allowed");
    }
    if params
        .get("clientId")
        .is_none_or(|id| id.is_empty() || id.len() > 128)
    {
        return problem(StatusCode::BAD_REQUEST, "clientId query param is required");
    }
    let Ok(permit) = host.connections.clone().try_acquire_owned() else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "Connection capacity reached",
        );
    };
    let stale = params.get("build") != Some(&host.build.build);
    let token = passport.read_cookie(&headers);
    ws.max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_upgrade(move |mut socket| async move {
            let _permit = permit;
            if stale {
                close(&mut socket, 4003, &host.build.build).await;
                return;
            }
            let Ok(Some(token)) = token else {
                close(&mut socket, 4001, "session not identified").await;
                return;
            };
            connected(socket, host, token).await;
        })
        .into_response()
}

async fn connected(mut socket: WebSocket, host: Host, token: String) {
    let mut revoked = host.revoked.subscribe();
    let reply = execute(
        &host,
        Invocation {
            operation_id: "socket-auth".into(),
            key: "identity.fetch".into(),
            payload: None,
            traceparent: None,
        },
        Some(token.clone()),
    )
    .await;
    let Some(session) = reply.session else {
        close(
            &mut socket,
            if reply.outcome.is_err() { 1000 } else { 4001 },
            "session not identified",
        )
        .await;
        return;
    };
    let mut connection = snap_runtime::transport::Connection::new(uuid::Uuid::new_v4().to_string());
    if send(
        &mut socket,
        json!({"key":"transport.epoch","payload":{"epoch":connection.epoch()}}),
    )
    .await
    .is_err()
    {
        return;
    }
    let expires = tokio::time::sleep(Duration::from_millis(
        session.expires_at.saturating_sub(passport::now()),
    ));
    tokio::pin!(expires);
    loop {
        tokio::select! {
            _ = &mut expires => { close(&mut socket, 4001, "session expired").await; break; }
            event = revoked.recv() => {
                match event {
                    Ok(ids) if !ids.contains(&session.session_id) => {},
                    _ => { close(&mut socket, 4001, "session ended").await; break; }
                }
            }
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break };
                let wire = match message { Message::Text(text) => text, Message::Close(_) => break, Message::Ping(bytes) => { if write(&mut socket, Message::Pong(bytes), Duration::from_secs(5)).await.is_err() { break; } continue; }, Message::Pong(_) => continue, _ => { close(&mut socket, 1003, "Text frames required").await; break; } };
                let Ok(invocation) = serde_json::from_str::<Invocation>(&wire) else { close(&mut socket, 1007, "Invalid Invocation").await; break };
                let target = invocation.operation_id.clone();
                let lane = host.operations.iter().find(|op| op.key == invocation.key).map(|op| op.lane);
                if let Err(error) = connection.admit(&invocation, lane) {
                    if send(&mut socket, serde_json::to_value(Completion::new(target, Err(error))).expect("completion")).await.is_err() { break; }
                    continue;
                }
                // Each frame is processed in receive order. The bounded host also
                // bounds accepted storage work. No client mutation uses this lane.
                let result = execute(&host, invocation, Some(token.clone())).await;
                if matches!(result.outcome, Err(Error::IdentityRequiredError { .. })) { close(&mut socket, 4001, "session ended").await; break; }
                if !matches!(result.outcome, Err(Error::InvalidInputError { .. } | Error::ContractViolationError { .. }))
                    && send(&mut socket, json!({"key":"transport.ack","target":target})).await.is_err() { break; }
                if send(&mut socket, serde_json::to_value(Completion::new(target, result.outcome)).expect("completion")).await.is_err() { break; }
            }
        }
    }
}

async fn send(socket: &mut WebSocket, value: serde_json::Value) -> Result<(), ()> {
    write(
        socket,
        Message::Text(value.to_string().into()),
        Duration::from_secs(5),
    )
    .await
}

// Every frame, including control frames, shares bounded physical delivery.
async fn write(socket: &mut WebSocket, message: Message, timeout: Duration) -> Result<(), ()> {
    tokio::time::timeout(timeout, socket.send(message))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}
async fn close(socket: &mut WebSocket, code: u16, reason: &str) {
    let mut end = reason.len().min(123);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    let reason = reason[..end].to_owned();
    let _ = write(
        socket,
        Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })),
        Duration::from_secs(1),
    )
    .await;
}
