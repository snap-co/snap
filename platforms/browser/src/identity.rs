//! Browser execution of the shared identity controller. Dropping the owner aborts
//! HTTP/timer work and detaches every socket callback before closing the socket.
use super::{Http, failure};
use futures_channel::{mpsc, oneshot};
use futures_util::{
    FutureExt, StreamExt,
    future::{AbortHandle, Abortable, LocalBoxFuture},
    stream::FuturesUnordered,
};
use snap_client::identity::{Action, Client as Core, Input, Snapshot};
use snap_protocol::{Outcome, Value};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};

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
    sender: mpsc::UnboundedSender<Event>,
    state: Rc<RefCell<Snapshot>>,
    sequence: Cell<u64>,
    abort: AbortHandle,
    finished: RefCell<Option<oneshot::Receiver<()>>>,
}

impl Client {
    pub fn new(http: Http, core: Core, changed: impl Fn(&Snapshot) + 'static) -> Self {
        let state = Rc::new(RefCell::new(core.snapshot()));
        let (sender, events) = mpsc::unbounded();
        let (abort, registration) = AbortHandle::new_pair();
        let (done, finished) = oneshot::channel();
        let work = drive(core, http, state.clone(), changed, sender.clone(), events);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = Abortable::new(work, registration).await;
            let _ = done.send(());
        });
        Self {
            sender,
            state,
            sequence: Cell::new(0),
            abort,
            finished: RefCell::new(Some(finished)),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state.borrow().clone()
    }
    pub async fn command(&self, key: String, payload: Option<Value>) -> Outcome {
        let id = self.sequence.get() + 1;
        self.sequence.set(id);
        let (reply, result) = oneshot::channel();
        self.sender
            .unbounded_send(Event::Command {
                id,
                key,
                payload,
                reply,
            })
            .map_err(|_| failure("Client is closed"))?;
        result.await.map_err(|_| failure("Client is closed"))?
    }
    pub async fn close(&self) {
        let _ = self.sender.unbounded_send(Event::Input(Input::Close));
        let done = self.finished.borrow_mut().take();
        if let Some(done) = done {
            let _ = done.await;
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

struct Socket {
    socket: web_sys::WebSocket,
    codec: Rc<RefCell<snap_web::Connection>>,
    _message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _close: Closure<dyn FnMut(web_sys::CloseEvent)>,
}
impl Drop for Socket {
    fn drop(&mut self) {
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
    }
}

async fn drive(
    mut core: Core,
    http: Http,
    state: Rc<RefCell<Snapshot>>,
    changed: impl Fn(&Snapshot),
    sender: mpsc::UnboundedSender<Event>,
    mut events: mpsc::UnboundedReceiver<Event>,
) {
    let mut replies = BTreeMap::new();
    let mut jobs: FuturesUnordered<LocalBoxFuture<'static, ()>> = FuturesUnordered::new();
    let mut socket: Option<Socket> = None;
    let mut input = Input::Start;
    loop {
        let closing = matches!(input, Input::Close);
        let mut actions = Vec::new();
        core.update(input, &mut actions);
        let snapshot = core.snapshot();
        *state.borrow_mut() = snapshot.clone();
        changed(&snapshot);
        for action in actions {
            match action {
                Action::Request {
                    generation,
                    invocation,
                } => {
                    let http = http.clone();
                    let sender = sender.clone();
                    jobs.push(
                        async move {
                            let result = http
                                .exchange(&invocation, snap_web::identity_method(&invocation.key))
                                .await;
                            if matches!(&result, Ok((409, _))) {
                                let _ = sender.unbounded_send(Event::Input(Input::BuildMismatch {
                                    generation,
                                }));
                                return;
                            }
                            let _ = sender.unbounded_send(Event::Input(Input::Completed {
                                generation,
                                result: result.and_then(|(_, body)| {
                                    snap_web::decode_completion(&body, &invocation.operation_id)
                                }),
                                id: invocation.operation_id,
                            }));
                        }
                        .boxed_local(),
                    );
                }
                Action::Connect { generation } => {
                    socket.take();
                    let url = web_sys::Url::new_with_base("/_transport/ws", &http.base)
                        .expect("base URL");
                    url.set_protocol(if url.protocol() == "https:" {
                        "wss:"
                    } else {
                        "ws:"
                    });
                    url.search_params().append("build", &http.build);
                    url.search_params().append(
                        "clientId",
                        &format!("browser-{}-{}", js_sys::Date::now(), js_sys::Math::random()),
                    );
                    match web_sys::WebSocket::new(&url.href()) {
                        Ok(ws) => {
                            let codec = Rc::new(RefCell::new(snap_web::Connection::default()));
                            let incoming = codec.clone();
                            let tx = sender.clone();
                            let received = Rc::new(Cell::new(false));
                            let ready = received.clone();
                            let message =
                                Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                                    if let Some(wire) = event.data().as_string() {
                                        ready.set(true);
                                        let _ = tx.unbounded_send(Event::Input(Input::Event {
                                            generation,
                                            event: incoming.borrow_mut().event(&wire),
                                        }));
                                    }
                                })
                                    as Box<dyn FnMut(_)>);
                            let tx = sender.clone();
                            let close = Closure::wrap(Box::new(move |event: web_sys::CloseEvent| {
                                let _ = tx.unbounded_send(Event::Input(Input::Disconnected {
                                    generation,
                                    reason: snap_web::disconnect(event.code()),
                                }));
                            })
                                as Box<dyn FnMut(_)>);
                            ws.set_onmessage(Some(message.as_ref().unchecked_ref()));
                            ws.set_onclose(Some(close.as_ref().unchecked_ref()));
                            let timeout_socket = ws.clone();
                            jobs.push(
                                async move {
                                    gloo_timers::future::TimeoutFuture::new(5_000).await;
                                    if !received.get() {
                                        let _ = timeout_socket.close();
                                    }
                                }
                                .boxed_local(),
                            );
                            socket = Some(Socket {
                                socket: ws,
                                codec,
                                _message: message,
                                _close: close,
                            });
                        }
                        Err(_) => {
                            let _ = sender.unbounded_send(Event::Input(Input::Disconnected {
                                generation,
                                reason: snap_protocol::Disconnect::Interrupted,
                            }));
                        }
                    }
                }
                Action::Send {
                    generation,
                    invocation,
                } => {
                    let sent = socket.as_ref().is_some_and(|ws| {
                        ws.codec
                            .borrow_mut()
                            .encode(invocation)
                            .is_ok_and(|wire| ws.socket.send_with_str(&wire).is_ok())
                    });
                    if !sent {
                        let _ = sender.unbounded_send(Event::Input(Input::Disconnected {
                            generation,
                            reason: snap_protocol::Disconnect::Interrupted,
                        }));
                    }
                }
                Action::Disconnect => {
                    socket.take();
                }
                Action::Retry {
                    generation,
                    milliseconds,
                } => {
                    let tx = sender.clone();
                    jobs.push(
                        async move {
                            gloo_timers::future::TimeoutFuture::new(milliseconds).await;
                            let _ = tx.unbounded_send(Event::Input(Input::Retry { generation }));
                        }
                        .boxed_local(),
                    );
                }
                Action::ReadDeadline { generation, id } => {
                    let tx = sender.clone();
                    jobs.push(
                        async move {
                            gloo_timers::future::TimeoutFuture::new(5_000).await;
                            let _ = tx.unbounded_send(Event::Input(Input::ReadTimeout {
                                generation,
                                id,
                            }));
                        }
                        .boxed_local(),
                    );
                }
                Action::Reload => {
                    if let Some(window) = web_sys::window() {
                        let _ = window.location().reload();
                    }
                }
                Action::Complete { id, outcome } => {
                    if let Some(reply) = replies.remove(&id) {
                        let _: Result<(), Outcome> = oneshot::Sender::send(reply, outcome);
                    }
                }
            }
        }
        // Dropping the owner futures cancels HTTP and timers after the portable
        // closed observation and pending-command failures have been published.
        if closing {
            return;
        }
        input = loop {
            futures_util::select! {
                event = events.next().fuse() => match event {
                    Some(Event::Input(input)) => break input,
                    Some(Event::Command { id, key, payload, reply }) => { replies.insert(id, reply); break Input::Command { id, key, payload }; }
                    None => return,
                },
                _ = jobs.select_next_some() => {},
            }
        };
    }
}
