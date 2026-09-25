//! Web host inside a Durable Object. One object is one application's transactional
//! authority domain, not a distributed transaction coordinator. Accepted futures
//! are retained with wait_until after their observer leaves; object/runtime loss
//! can still interrupt them. Mutations are never automatically replayed.
use crate::{crypto, fifo::Fifo};
use futures_channel::oneshot;
use futures_util::{
    StreamExt,
    future::{Either, select},
};
use serde::{Deserialize, Serialize};
use snap_protocol::{Error as ProtocolError, Invocation, Provider, json};
use snap_web::{Binding, Completion, Method, Reply, cookie::Cookie};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    time::Duration,
};
use worker::{Delay, Request, Response, Result, State, WebSocket, WebSocketPair};

pub struct Config {
    pub application: String,
    pub build: String,
    pub origin: String,
    pub bindings: Vec<Binding>,
    pub cookie: Cookie,
    pub identify: &'static str,
}

pub struct Host<P> {
    provider: RefCell<P>,
    state: Rc<State>,
    config: Config,
    admitted: Rc<Cell<usize>>,
    queued_frames: Rc<Cell<usize>>,
    socket_jobs: RefCell<BTreeMap<String, Rc<Fifo>>>,
}
struct Permit(Rc<Cell<usize>>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

#[derive(Serialize, Deserialize)]
struct Attachment {
    build: String,
    token: String,
    session: String,
    expires: u64,
    connection: snap_runtime::transport::Connection,
}

impl<P: Provider<Context = Option<String>, Output = Reply> + 'static> Host<P> {
    pub fn new(provider: P, state: Rc<State>, mut config: Config) -> Result<Self> {
        let origin = worker::Url::parse(&config.origin)?;
        if !matches!(origin.scheme(), "http" | "https")
            || origin.host_str().is_none()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
        {
            return Err(worker::Error::RustError("Invalid public origin".into()));
        }
        config.origin = origin.origin().ascii_serialization();
        let operations: Vec<_> = provider.operations().map(|op| op.key).collect();
        if !operations.contains(&config.identify)
            || config.bindings.iter().enumerate().any(|(i, b)| {
                !operations.contains(&b.key)
                    || config.bindings[..i].iter().any(|other| other.key == b.key)
            })
        {
            return Err(worker::Error::RustError("Invalid web bindings".into()));
        }
        for socket in state.get_websockets() {
            match socket.deserialize_attachment::<Attachment>() {
                Ok(Some(data)) if data.build == config.build => {}
                _ => {
                    socket.close(Some(4003), Some(&config.build))?;
                }
            }
        }
        Ok(Self {
            provider: RefCell::new(provider),
            state,
            config,
            admitted: Rc::new(Cell::new(0)),
            queued_frames: Rc::new(Cell::new(0)),
            socket_jobs: RefCell::new(BTreeMap::new()),
        })
    }

    async fn execute(&self, invocation: Invocation, token: Option<String>) -> Reply {
        let fail = |message: &str| {
            Reply::new(Err(ProtocolError::UnavailableError {
                message: message.into(),
            }))
        };
        if self.admitted.get() >= 64 {
            return fail("Host is at capacity");
        }
        self.admitted.set(self.admitted.get() + 1);
        let permit = Permit(self.admitted.clone());
        let future = self.provider.borrow_mut().invoke(invocation, token);
        let (send, receive) = oneshot::channel();
        let state = self.state.clone();
        self.state.wait_until(async move {
            let _permit = permit;
            let reply = future.await;
            if !reply.terminate.is_empty() {
                for socket in state.get_websockets() {
                    if let Ok(Some(data)) = socket.deserialize_attachment::<Attachment>()
                        && reply.terminate.contains(&data.session)
                    {
                        let _ = socket.close(Some(4001), Some("session ended"));
                    }
                }
            }
            let _ = send.send(reply);
        });
        match select(receive, Box::pin(Delay::from(Duration::from_secs(5)))).await {
            Either::Left((Ok(reply), _)) => reply,
            _ => fail("Host did not complete the operation"),
        }
    }

    fn token(&self, request: &Request) -> std::result::Result<Option<String>, ProtocolError> {
        let cookie = request.headers().get("cookie").ok().flatten();
        self.config.cookie.read(cookie.as_deref())
    }

    pub async fn fetch(&self, mut request: Request) -> Result<Response> {
        if request.path() == "/__snap/build" {
            return Response::from_json(
                &json!({"contract":1,"application":self.config.application,"build":self.config.build}),
            );
        }
        if request.path() == "/_transport/ws" {
            return self.upgrade(request).await;
        }
        let binding = self
            .config
            .bindings
            .iter()
            .find(|b| request.path() == format!("/{}", b.key.replace('.', "/")));
        let Some(binding) = binding.filter(|b| b.http.is_some()) else {
            return problem("Not found", 404);
        };
        let method = binding.http.expect("http binding");
        if request.method()
            != match method {
                Method::Get => worker::Method::Get,
                Method::Post => worker::Method::Post,
            }
        {
            return problem("Method not allowed", 405);
        }
        if request.headers().get("x-snap-build")?.as_deref() != Some(&self.config.build) {
            return problem("Snap Build mismatch", 409);
        }
        if method == Method::Post
            && request
                .headers()
                .get("origin")?
                .is_some_and(|origin| origin != self.config.origin)
        {
            return problem("Origin not allowed", 403);
        }
        let target = request
            .headers()
            .get("x-snap-operation-id")?
            .unwrap_or_else(|| "http".into());
        let token = match self.token(&request) {
            Ok(token) => token,
            Err(error) => return self.complete(target, Reply::new(Err(error))),
        };
        let payload = if method == Method::Get {
            let fields = request
                .url()?
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), serde_json::Value::String(v.into_owned())))
                .collect::<serde_json::Map<_, _>>();
            (!fields.is_empty()).then_some(fields.into())
        } else {
            let mut body = Vec::new();
            if let Ok(mut stream) = request.stream() {
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    if body.len() + chunk.len() > 64 * 1024 {
                        return problem("Body too large", 413);
                    }
                    body.extend(chunk);
                }
            }
            if body.is_empty() {
                None
            } else {
                match serde_json::from_slice(&body) {
                    Ok(value) => Some(value),
                    Err(_) => return problem("Malformed body", 400),
                }
            }
        };
        let invocation = Invocation {
            operation_id: target.clone(),
            key: binding.key.into(),
            payload,
            traceparent: request.headers().get("traceparent")?,
        };
        self.complete(target, self.execute(invocation, token).await)
    }

    fn complete(&self, target: String, reply: Reply) -> Result<Response> {
        let status = match &reply.outcome {
            Ok(_) => 200,
            Err(
                ProtocolError::InvalidInputError { .. }
                | ProtocolError::ContractViolationError { .. },
            ) => 400,
            Err(ProtocolError::IdentityRequiredError { .. }) => 401,
            Err(ProtocolError::IdentityForbiddenError { .. }) => 403,
            Err(ProtocolError::UnavailableError { .. }) => 503,
            _ => 400,
        };
        let mut event = Completion::new(target, reply.outcome);
        if reply.empty {
            event = event.empty();
        }
        if reply.cookie.is_some() {
            event = event.session_changed();
        }
        let mut response = Response::from_json(&event)?.with_status(status);
        response
            .headers_mut()
            .set("cache-control", "private, no-store")?;
        if let Some(token) = reply.cookie {
            response
                .headers_mut()
                .set("set-cookie", &self.config.cookie.encode(token.as_deref()))?;
        }
        Ok(response)
    }

    async fn identify(&self, token: String) -> Reply {
        self.execute(
            Invocation {
                operation_id: "socket-auth".into(),
                key: self.config.identify.into(),
                payload: None,
                traceparent: None,
            },
            Some(token),
        )
        .await
    }

    async fn upgrade(&self, request: Request) -> Result<Response> {
        if request.method() != worker::Method::Get
            || request
                .headers()
                .get("upgrade")?
                .is_none_or(|v| !v.eq_ignore_ascii_case("websocket"))
        {
            return Response::error("WebSocket required", 426);
        }
        if request.headers().get("origin")?.as_deref() != Some(&self.config.origin) {
            return Response::error("Origin not allowed", 403);
        }
        let params: BTreeMap<_, _> = request
            .url()?
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if params
            .get("clientId")
            .is_none_or(|v| v.is_empty() || v.len() > 128)
        {
            return Response::error("clientId query param is required", 400);
        }
        if self.state.get_websockets().len() >= 128 {
            return Response::error("Connection capacity reached", 503);
        }
        let pair = WebSocketPair::new()?;
        self.state.accept_web_socket(&pair.server);
        let response = Response::from_websocket(pair.client)?;
        if params.get("build") != Some(&self.config.build) {
            pair.server.close(Some(4003), Some(&self.config.build))?;
            return Ok(response);
        }
        let Ok(Some(token)) = self.token(&request) else {
            pair.server
                .close(Some(4001), Some("session not identified"))?;
            return Ok(response);
        };
        let reply = self.identify(token.clone()).await;
        let Some(lease) = reply.lease else {
            pair.server.close(
                Some(if reply.outcome.is_err() { 1000 } else { 4001 }),
                Some("session not identified"),
            )?;
            return Ok(response);
        };
        let connection = snap_runtime::transport::Connection::new(
            crypto::id().map_err(|_| worker::Error::RustError("Randomness unavailable".into()))?,
        );
        pair.server.serialize_attachment(Attachment {
            build: self.config.build.clone(),
            token: token.clone(),
            session: lease.id,
            expires: lease.expires_at,
            connection,
        })?;
        // Attachment is now visible to revocation. Recheck authority to close the
        // gap between initial resolution and attaching the session identifier.
        if self.identify(token).await.lease.is_none() {
            pair.server.close(Some(4001), Some("session ended"))?;
            return Ok(response);
        }
        let data = pair
            .server
            .deserialize_attachment::<Attachment>()?
            .expect("attachment");
        pair.server
            .send(&json!({"key":"transport.epoch","payload":{"epoch":data.connection.epoch()}}))?;
        self.schedule_expiry().await?;
        Ok(response)
    }

    pub async fn message(
        &self,
        socket: WebSocket,
        message: worker::WebSocketIncomingMessage,
    ) -> Result<()> {
        // Hibernation callbacks can overlap while an earlier frame awaits work.
        // Bound the waiting frames as well as the executing invocations.
        if self.queued_frames.get() >= 64 {
            return socket.close(Some(1013), Some("Frame capacity reached"));
        }
        self.queued_frames.set(self.queued_frames.get() + 1);
        let _frame = Permit(self.queued_frames.clone());
        let Some(data) = socket.deserialize_attachment::<Attachment>()? else {
            return socket.close(Some(4001), Some("session not identified"));
        };
        let epoch = data.connection.epoch().to_owned();
        let queue = self
            .socket_jobs
            .borrow_mut()
            .entry(epoch)
            .or_default()
            .clone();
        let _turn = queue.enter().await;
        let Some(mut data) = socket.deserialize_attachment::<Attachment>()? else {
            return Ok(());
        };
        if data.expires <= crypto::now() {
            return socket.close(Some(4001), Some("session expired"));
        }
        let worker::WebSocketIncomingMessage::String(wire) = message else {
            return socket.close(Some(1003), Some("Text frames required"));
        };
        if wire.len() > 64 * 1024 {
            return socket.close(Some(1009), Some("Frame too large"));
        }
        let Ok(invocation) = serde_json::from_str::<Invocation>(&wire) else {
            return socket.close(Some(1007), Some("Invalid Invocation"));
        };
        let target = invocation.operation_id.clone();
        let eligible = self
            .config
            .bindings
            .iter()
            .any(|b| b.key == invocation.key && b.socket);
        let admission = if eligible {
            data.connection.admit(snap_web::sequence(&target))
        } else {
            Err(ProtocolError::ContractViolationError {
                message: "Operation is not registered for this carrier".into(),
            })
        };
        if let Err(error) = admission {
            return socket.send(&Completion::new(target, Err(error)));
        }
        socket.serialize_attachment(&data)?;
        let reply = self.execute(invocation, Some(data.token)).await;
        if matches!(
            reply.outcome,
            Err(ProtocolError::IdentityRequiredError { .. })
        ) {
            return socket.close(Some(4001), Some("session ended"));
        }
        if !matches!(
            reply.outcome,
            Err(ProtocolError::InvalidInputError { .. }
                | ProtocolError::ContractViolationError { .. })
        ) {
            socket.send(&json!({"key":"transport.ack","target":target}))?;
        }
        socket.send(&Completion::new(target, reply.outcome))
    }

    pub fn closed(&self, socket: WebSocket) {
        if let Ok(Some(data)) = socket.deserialize_attachment::<Attachment>() {
            self.socket_jobs
                .borrow_mut()
                .remove(data.connection.epoch());
        }
    }

    pub async fn expire(&self) -> Result<()> {
        self.schedule_expiry().await
    }
    async fn schedule_expiry(&self) -> Result<()> {
        let now = crypto::now();
        let mut next: Option<u64> = None;
        for socket in self.state.get_websockets() {
            if let Some(data) = socket.deserialize_attachment::<Attachment>()? {
                if data.expires <= now {
                    socket.close(Some(4001), Some("session expired"))?;
                } else {
                    next = Some(next.map_or(data.expires, |previous| previous.min(data.expires)));
                }
            }
        }
        if let Some(next) = next {
            self.state.storage().set_alarm(next as i64).await?;
        }
        Ok(())
    }
}

fn problem(message: &str, status: u16) -> Result<Response> {
    let mut response = Response::from_json(&json!({"error":message}))?.with_status(status);
    response.headers_mut().set("cache-control", "no-store")?;
    Ok(response)
}
