//! Native host execution and web carriers. Composition supplies a Protocol provider.
pub mod client;
pub mod cookie;
pub mod files;
mod http;
pub mod oidc;
pub mod outgoing;
pub mod passport;
pub mod store;
mod websocket;
pub use snap_web as web;
pub use snap_web::{Lease, Reply};

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, RawQuery, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::Serialize;
use snap_protocol::{Error, Invocation, Operation, Outcome, Provider, Value, json};
use snap_web::{Binding, Completion, Method};
use std::{io, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot},
};

const CAPACITY: usize = 64;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Config {
    pub address: SocketAddr,
    pub application: String,
    pub build: String,
    pub web_dir: Option<PathBuf>,
}
impl Config {
    pub fn from_env(application: &str) -> io::Result<Self> {
        let address = std::env::var("SNAP_ADDR").unwrap_or_else(|_| "127.0.0.1:3846".into());
        Ok(Self {
            address: address
                .parse()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
            application: application.into(),
            build: std::env::var("SNAP_BUILD").unwrap_or_else(|_| "rust-spike".into()),
            web_dir: std::env::var_os("SNAP_WEB_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    let path = std::env::current_exe().ok()?.parent()?.join("web");
                    path.is_dir().then_some(path)
                }),
        })
    }
}
#[derive(Clone, Serialize)]
struct Build {
    contract: u8,
    application: String,
    build: String,
}

/// Web projection selected by application composition. The core provider need
/// not use cookies, or expose any of these bindings through another carrier.
pub struct Web {
    pub bindings: Vec<Binding>,
    pub session: Option<SessionCarrier>,
}
#[derive(Clone)]
pub struct SessionCarrier {
    pub origin: String,
    pub cookie: cookie::Cookie,
    pub identify: &'static str,
}
struct Work {
    invocation: Invocation,
    token: Option<String>,
    reply: oneshot::Sender<Reply>,
    permit: OwnedSemaphorePermit,
    accepted: Option<oneshot::Sender<()>>,
}
#[derive(Clone)]
struct Host {
    inbox: mpsc::Sender<Work>,
    capacity: Arc<Semaphore>,
    build: Build,
    session: Option<SessionCarrier>,
    revoked: broadcast::Sender<Vec<String>>,
    bindings: Arc<Vec<Binding>>,
    connections: Arc<Semaphore>,
}

struct Plain<M>(M);
impl<M: Provider<Context = (), Output = Outcome>> Provider for Plain<M> {
    type Context = Option<String>;
    type Output = Reply;
    fn operations(&self) -> impl Iterator<Item = Operation> {
        self.0.operations()
    }
    fn prepare(
        &mut self,
        invocation: Invocation,
        _: Option<String>,
    ) -> impl core::future::Future<Output = Result<snap_protocol::Accepted<Reply>, Error>> + 'static
    {
        let future = snap_protocol::dispatch(&mut self.0, invocation, ());
        async move { Ok(future.await?.map(Reply::new)) }
    }
}
/// Convenience composition for the existing HTTP-only Healthy consumers.
pub fn run(
    module: impl Provider<Context = (), Output = Outcome> + 'static,
    config: Config,
) -> io::Result<()> {
    let bindings = module.operations().map(|op| Binding::get(op.key)).collect();
    run_application(
        Plain(module),
        config,
        Web {
            bindings,
            session: None,
        },
    )
}
pub fn run_application(
    module: impl Provider<Context = Option<String>, Output = Reply> + 'static,
    config: Config,
    web: Web,
) -> io::Result<()> {
    run_with_http(module, config, web, None)
}
/// Compose standards endpoints alongside the selected Snap carrier bindings.
pub fn run_application_with_http(
    module: impl Provider<Context = Option<String>, Output = Reply> + 'static,
    config: Config,
    web: Web,
    http: impl snap_http::Service,
) -> io::Result<()> {
    run_with_http(module, config, web, Some(Box::new(http)))
}
fn run_with_http(
    module: impl Provider<Context = Option<String>, Output = Reply> + 'static,
    config: Config,
    web: Web,
    http: Option<Box<dyn snap_http::Service>>,
) -> io::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(tokio::task::LocalSet::new().run_until(serve(module, config, web, http)))
}
async fn serve(
    module: impl Provider<Context = Option<String>, Output = Reply> + 'static,
    config: Config,
    mut web: Web,
    http: Option<Box<dyn snap_http::Service>>,
) -> io::Result<()> {
    let operations: Vec<_> = module.operations().map(|op| op.key).collect();
    for (i, binding) in web.bindings.iter().enumerate() {
        if !operations.contains(&binding.key)
            || web.bindings[..i].iter().any(|b| b.key == binding.key)
        {
            return Err(io::Error::other("Invalid or duplicate carrier binding"));
        }
    }
    let listener = TcpListener::bind(config.address).await?;
    if let Some(session) = &mut web.session {
        if !operations.contains(&session.identify) {
            return Err(io::Error::other("Unknown session resolver"));
        }
        let mut origin = parse_origin(&session.origin)?;
        if origin.port() == Some(0) {
            let _ = origin.set_port(Some(listener.local_addr()?.port()));
        }
        session.origin = origin.origin().ascii_serialization();
    }
    let (inbox, receiver) = mpsc::channel(CAPACITY);
    let host = Host {
        inbox,
        capacity: Arc::new(Semaphore::new(CAPACITY)),
        build: Build {
            contract: 1,
            application: config.application,
            build: config.build,
        },
        session: web.session,
        revoked: broadcast::channel(64).0,
        bindings: Arc::new(web.bindings),
        connections: Arc::new(Semaphore::new(128)),
    };
    let mut router = Router::new().route("/__snap/build", get(build));
    if host.session.is_some() && host.bindings.iter().any(|b| b.socket) {
        router = router.route("/_transport/ws", get(websocket::upgrade));
    }
    for binding in host.bindings.iter().copied() {
        let Some(method) = binding.http else { continue };
        let route = match method {
            Method::Get => get(request)
                .head(|| async { problem(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed") }),
            Method::Post => post(request),
        }
        .fallback(|| async { problem(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed") })
        .layer(Extension(binding));
        router = router.route(&format!("/{}", binding.key.replace('.', "/")), route);
    }
    let router = if let Some(web_dir) = config.web_dir {
        if !web_dir.join("index.html").is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "SNAP_WEB_DIR has no index.html; build the web application first",
            ));
        }
        router.fallback_service(tower_http::set_header::SetResponseHeader::if_not_present(
            tower_http::services::ServeDir::new(web_dir),
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-cache"),
        ))
    } else {
        router.fallback(|| async { problem(StatusCode::NOT_FOUND, "Not found") })
    }
    .layer(DefaultBodyLimit::max(64 * 1024))
    .with_state(host.clone());
    let (router, http_worker) = if let Some(service) = http {
        let (extra, task) = self::http::router(service, host.revoked.clone());
        (router.merge(extra), Some(task))
    } else {
        (router, None)
    };
    eprintln!("listening on http://{}", listener.local_addr()?);
    let worker = tokio::task::spawn_local(drive(module, receiver, host));
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await;
    worker.abort();
    if let Some(task) = http_worker {
        task.abort();
        let _ = task.await;
    }
    let _ = worker.await;
    result
}
async fn shutdown() {
    #[cfg(unix)]
    if let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}};
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// One owner creates and polls continuations. Each admitted invocation retains
/// its permit until completion, even after its observer times out or disconnects.
/// Correlation uses private response slots rather than caller-chosen operation IDs.
/// Domain output projection belongs to composition, not this scheduler.
async fn drive(
    mut module: impl Provider<Context = Option<String>, Output = Reply>,
    mut inbox: mpsc::Receiver<Work>,
    host: Host,
) {
    let mut jobs = FuturesUnordered::new();
    loop {
        tokio::select! {
            work=inbox.recv()=>{
                let Some(work)=work else {break};
                if work.reply.is_closed() {continue;}
                let preparation=snap_protocol::dispatch(&mut module,work.invocation,work.token);
                jobs.push(async move {
                    let _permit=work.permit;
                    let result = match preparation.await {
                        Ok(accepted) => accepted.start(|| { if let Some(signal) = work.accepted { let _ = signal.send(()); } }).await,
                        Err(error) => Reply::new(Err(error)),
                    };
                    (work.reply,result)
                });
            }
            completed=jobs.next(),if !jobs.is_empty()=>{
                if let Some((observer,reply))=completed {
                    if !reply.terminate.is_empty() {let _=host.revoked.send(reply.terminate.clone());}
                    let _=observer.send(reply);
                }
            }
        }
    }
}
async fn build(State(host): State<Host>) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(host.build)).into_response()
}
async fn request(
    State(host): State<Host>,
    Extension(binding): Extension<Binding>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    if binding.key != "health.up"
        && headers.get("x-snap-build").and_then(|v| v.to_str().ok())
            != Some(host.build.build.as_str())
    {
        return problem(StatusCode::CONFLICT, "Snap Build mismatch");
    }
    let operation_id = headers
        .get("x-snap-operation-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let payload = match binding.http {
        Some(Method::Get) => {
            let fields = form_urlencoded::parse(query.as_deref().unwrap_or_default().as_bytes())
                .map(|(k, v)| (k.into_owned(), Value::String(v.into_owned())))
                .collect::<serde_json::Map<_, _>>();
            (!fields.is_empty()).then_some(Value::Object(fields))
        }
        Some(Method::Post) if body.is_empty() => None,
        Some(Method::Post) => match serde_json::from_slice(&body) {
            Ok(value) => Some(value),
            Err(_) => return problem(StatusCode::BAD_REQUEST, "Malformed body"),
        },
        None => unreachable!(),
    };
    let token = if let Some(session) = &host.session {
        if binding.http == Some(Method::Post)
            && headers
                .get("origin")
                .is_some_and(|v| v.to_str().ok() != Some(session.origin.as_str()))
        {
            return problem(StatusCode::FORBIDDEN, "Origin not allowed");
        }
        match session.cookie.read(&headers) {
            Ok(token) => token,
            Err(error) => return completion(operation_id, Err(error)),
        }
    } else {
        None
    };
    let invocation = Invocation {
        operation_id: operation_id.clone(),
        key: binding.key.into(),
        payload,
        traceparent: headers
            .get("traceparent")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    };
    let result = execute(&host, invocation, token).await;
    let status = status(&result.outcome);
    let mut event = Completion::new(operation_id, result.outcome);
    if result.empty {
        event = event.empty();
    }
    if result.cookie.is_some() {
        event = event.session_changed();
    }
    let mut response = (
        status,
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(event),
    )
        .into_response();
    if let (Some(session), Some(token)) = (&host.session, result.cookie)
        && let Ok(value) = session.cookie.encode(token.as_deref()).parse()
    {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}
async fn execute(host: &Host, invocation: Invocation, token: Option<String>) -> Reply {
    execute_observed(host, invocation, token, None).await
}
async fn execute_observed(
    host: &Host,
    invocation: Invocation,
    token: Option<String>,
    accepted: Option<oneshot::Sender<()>>,
) -> Reply {
    let failed = |message: &str| {
        Reply::new(Err(Error::UnavailableError {
            message: message.into(),
        }))
    };
    let Ok(permit) = host.capacity.clone().try_acquire_owned() else {
        return failed("Host is at capacity");
    };
    let (reply, response) = oneshot::channel();
    if host
        .inbox
        .try_send(Work {
            invocation,
            token,
            reply,
            permit,
            accepted,
        })
        .is_err()
    {
        return failed("Host is not accepting work");
    }
    match tokio::time::timeout(RESPONSE_TIMEOUT, response).await {
        Ok(Ok(response)) => response,
        _ => failed("Host did not complete the operation"),
    }
}
fn completion(operation_id: String, outcome: Outcome) -> Response {
    (
        status(&outcome),
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(Completion::new(operation_id, outcome)),
    )
        .into_response()
}
fn status(outcome: &Outcome) -> StatusCode {
    match outcome {
        Ok(_) => StatusCode::OK,
        Err(Error::ContractViolationError { message }) if message.starts_with("Unknown key:") => {
            StatusCode::NOT_FOUND
        }
        Err(Error::InvalidInputError { .. } | Error::ContractViolationError { .. }) => {
            StatusCode::BAD_REQUEST
        }
        Err(Error::UnavailableError { .. }) => StatusCode::SERVICE_UNAVAILABLE,
        Err(Error::IdentityRequiredError { .. }) => StatusCode::UNAUTHORIZED,
        Err(Error::IdentityForbiddenError { .. }) => StatusCode::FORBIDDEN,
        Err(Error::OperationError { .. } | Error::IndeterminateError { .. }) => {
            StatusCode::BAD_REQUEST
        }
    }
}
fn problem(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"error":message})),
    )
        .into_response()
}
/// Shared normalization for carrier origin checks and composition's cookie policy.
pub fn parse_origin(value: &str) -> io::Result<reqwest::Url> {
    let origin = reqwest::Url::parse(value).map_err(io::Error::other)?;
    if !matches!(origin.scheme(), "http" | "https")
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.host_str().is_none()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(io::Error::other("SNAP_ORIGIN must be an HTTP(S) origin"));
    }
    Ok(origin)
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
