//! Native identity SDK. The portable controller owns commands and reconnect policy.
use super::{Http, unavailable};
use futures_util::{SinkExt, StreamExt};
use reqwest::cookie::CookieStore;
use snap_client::identity::{Action, Client as Core, Input, Snapshot};
use snap_protocol::{Error, Outcome, Value, json};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

enum Event {
    Input(Input),
    Command {
        id: u64,
        key: String,
        payload: Option<Value>,
        reply: oneshot::Sender<Outcome>,
    },
}

pub struct Client {
    sender: mpsc::Sender<Event>,
    state: watch::Receiver<Snapshot>,
    sequence: AtomicU64,
    task: tokio::task::JoinHandle<()>,
    finished: watch::Receiver<bool>,
}

impl Client {
    pub fn new(http: Http, core: Core) -> Self {
        let (state, receiver) = watch::channel(core.snapshot());
        let (sender, events) = mpsc::channel(128);
        let (done, finished) = watch::channel(false);
        let work = drive(core, http, state, sender.clone(), events);
        let task = tokio::spawn(async move {
            work.await;
            done.send_replace(true);
        });
        Self {
            sender,
            state: receiver,
            sequence: AtomicU64::new(1),
            task,
            finished,
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state.borrow().clone()
    }
    pub fn observe(&self) -> watch::Receiver<Snapshot> {
        self.state.clone()
    }
    pub async fn command(&self, key: &str, payload: Option<Value>) -> Outcome {
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(Event::Command {
                id: self.sequence.fetch_add(1, Ordering::Relaxed),
                key: key.into(),
                payload,
                reply,
            })
            .map_err(|_| unavailable("Client is closed or busy"))?;
        result.await.map_err(unavailable)?
    }
    pub async fn sign_in(&self, kind: &str, email: &str, password: &str) -> Outcome {
        self.command(
            "identity.password.acquire",
            Some(json!({"kind":kind,"email":email,"password":password})),
        )
        .await
    }
    pub async fn release(&self, scope: snap_protocol::identity::Release) -> Outcome {
        self.command(
            "identity.release",
            Some(serde_json::to_value(scope).map_err(unavailable)?),
        )
        .await
    }
    pub async fn refresh(&self) -> Outcome {
        self.command("refresh", None).await
    }
    pub async fn close(&self) {
        let _ = self.sender.send(Event::Input(Input::Close)).await;
        let mut finished = self.finished.clone();
        let _ = finished.wait_for(|done| *done).await;
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn drive(
    mut core: Core,
    http: Http,
    state: watch::Sender<Snapshot>,
    sender: mpsc::Sender<Event>,
    mut events: mpsc::Receiver<Event>,
) {
    let mut replies = BTreeMap::new();
    let mut jobs = JoinSet::new();
    let mut socket: Option<(mpsc::Sender<String>, tokio::task::AbortHandle)> = None;
    let mut input = Input::Start;
    loop {
        let closing = matches!(input, Input::Close);
        let mut actions = Vec::new();
        core.update(input, &mut actions);
        state.send_replace(core.snapshot());
        for action in actions {
            match action {
                Action::Http {
                    generation,
                    invocation,
                    lane,
                } => {
                    let http = http.clone();
                    let sender = sender.clone();
                    jobs.spawn(async move {
                        let result = http.request(&invocation, lane).await;
                        let _ = sender
                            .send(Event::Input(Input::Http {
                                generation,
                                id: invocation.operation_id,
                                result,
                            }))
                            .await;
                    });
                }
                Action::Connect { generation } => {
                    if let Some((_, task)) = socket.take() {
                        task.abort();
                    }
                    let (outbound, receive) = mpsc::channel(32);
                    let task = jobs.spawn(connection(
                        http.clone(),
                        generation,
                        sender.clone(),
                        receive,
                    ));
                    socket = Some((outbound, task));
                }
                Action::Send {
                    generation,
                    invocation,
                } => {
                    let sent = socket.as_ref().is_some_and(|(socket, _)| {
                        socket
                            .try_send(serde_json::to_string(&invocation).expect("invocation"))
                            .is_ok()
                    });
                    if !sent {
                        let _ = sender.try_send(Event::Input(Input::Disconnected {
                            generation,
                            code: 1006,
                        }));
                    }
                }
                Action::Disconnect => {
                    if let Some((_, task)) = socket.take() {
                        task.abort();
                    }
                }
                Action::Retry {
                    generation,
                    milliseconds,
                } => {
                    let sender = sender.clone();
                    jobs.spawn(async move {
                        tokio::time::sleep(Duration::from_millis(milliseconds.into())).await;
                        let _ = sender.send(Event::Input(Input::Retry { generation })).await;
                    });
                }
                Action::ReadDeadline { generation, id } => {
                    let sender = sender.clone();
                    jobs.spawn(async move {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        let _ = sender
                            .send(Event::Input(Input::ReadTimeout { generation, id }))
                            .await;
                    });
                }
                Action::Reload => {
                    let _ = sender.try_send(Event::Input(Input::Close));
                }
                Action::Complete { id, outcome } => {
                    if let Some(reply) = replies.remove(&id) {
                        let _: Result<(), Outcome> = oneshot::Sender::send(reply, outcome);
                    }
                }
            }
        }
        if closing {
            break;
        }
        input = loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(Event::Input(input)) => break input,
                    Some(Event::Command { id, key, payload, reply }) => { replies.insert(id, reply); break Input::Command { id, key, payload }; }
                    None => break Input::Close,
                },
                _ = jobs.join_next(), if !jobs.is_empty() => {},
            }
        };
    }
    jobs.abort_all();
    while jobs.join_next().await.is_some() {}
    for (_, reply) in replies {
        let _ = reply.send(Err(unavailable("Client is closed")));
    }
}

async fn connection(
    http: Http,
    generation: u64,
    sender: mpsc::Sender<Event>,
    mut outbound: mpsc::Receiver<String>,
) {
    let result = async {
        let mut url = http.base.join("/_transport/ws").map_err(unavailable)?;
        url.set_scheme(if http.base.scheme() == "https" { "wss" } else { "ws" }).map_err(|_| unavailable("Invalid URL"))?;
        url.query_pairs_mut().append_pair("build", &http.build).append_pair("clientId", &uuid::Uuid::new_v4().to_string());
        let mut request = url.as_str().into_client_request().map_err(unavailable)?;
        request.headers_mut().insert("origin", http.base.origin().ascii_serialization().parse().map_err(unavailable)?);
        if let Some(cookie) = http.jar.cookies(&http.base) { request.headers_mut().insert("cookie", cookie); }
        let (mut ws, _) = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::connect_async(request)).await.map_err(unavailable)?.map_err(unavailable)?;
        let first = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.map_err(unavailable)?;
        match first {
            Some(Ok(Message::Text(wire))) => { sender.send(Event::Input(Input::Frame { generation, wire: wire.to_string() })).await.map_err(unavailable)?; }
            Some(Ok(Message::Close(detail))) => return Ok(detail.map(|d| u16::from(d.code)).unwrap_or(1000)),
            _ => return Ok(1006),
        }
        loop {
            tokio::select! {
                wire = outbound.recv() => { let Some(wire) = wire else { let _ = ws.close(None).await; return Ok(1000) }; ws.send(Message::Text(wire.into())).await.map_err(unavailable)?; }
                frame = ws.next() => match frame {
                    Some(Ok(Message::Text(wire))) => { sender.send(Event::Input(Input::Frame { generation, wire: wire.to_string() })).await.map_err(unavailable)?; }
                    Some(Ok(Message::Close(detail))) => return Ok(detail.map(|d| u16::from(d.code)).unwrap_or(1000)),
                    Some(Ok(_)) => {},
                    _ => return Ok(1006),
                }
            }
        }
    }.await as Result<u16, Error>;
    let _ = sender
        .send(Event::Input(Input::Disconnected {
            generation,
            code: result.unwrap_or(1006),
        }))
        .await;
}
