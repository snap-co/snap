//! Native execution and HTTP IO. All Tokio, Axum, OS, and environment access lives here.

pub mod client;
pub mod passport;
mod websocket;

use std::{
    collections::BTreeMap,
    io,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, RawQuery, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use snap_protocol::{Completion, Error, Invocation, Lane, Operation, Outcome, Value, json};
use snap_runtime::{Action, Delivery, Input, Module};
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot},
    task::JoinSet,
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
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
            application: application.into(),
            build: std::env::var("SNAP_BUILD").unwrap_or_else(|_| "rust-spike".into()),
            web_dir: std::env::var_os("SNAP_WEB_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    let beside_binary = std::env::current_exe().ok()?.parent()?.join("web");
                    beside_binary.is_dir().then_some(beside_binary)
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

struct Work {
    delivery: Delivery,
    invocation: Invocation,
    context: snap_runtime::passport::Context,
    reply: oneshot::Sender<Reply>,
    permit: OwnedSemaphorePermit,
}

struct Reply {
    outcome: Outcome,
    empty: bool,
    session: Option<snap_protocol::identity::Session>,
    cookie: Option<Option<String>>,
}

struct Pending {
    reply: oneshot::Sender<Reply>,
    session: Option<snap_protocol::identity::Session>,
    cookie: Option<Option<String>>,
    _permit: OwnedSemaphorePermit,
}

#[derive(Clone)]
struct Host {
    inbox: mpsc::Sender<Work>,
    next_delivery: Arc<AtomicU64>,
    capacity: Arc<Semaphore>,
    build: Build,
    passport: Option<passport::Passport>,
    revoked: broadcast::Sender<Vec<String>>,
    operations: Arc<Vec<Operation>>,
    connections: Arc<Semaphore>,
}

pub fn run(module: impl Module + Send + 'static, config: Config) -> io::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(module, config, None))
}

pub fn run_with_passport(
    module: impl Module + Send + 'static,
    config: Config,
    passport: passport::Passport,
) -> io::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(module, config, Some(passport)))
}

async fn serve(
    module: impl Module + Send + 'static,
    config: Config,
    mut passport: Option<passport::Passport>,
) -> io::Result<()> {
    let listener = TcpListener::bind(config.address).await?;
    if let Some(passport) = &mut passport {
        let mut origin = reqwest::Url::parse(&passport.origin).map_err(io::Error::other)?;
        if origin.port() == Some(0) {
            let _ = origin.set_port(Some(listener.local_addr()?.port()));
            passport.origin = origin.origin().ascii_serialization();
        }
    }
    let (inbox, receiver) = mpsc::channel(CAPACITY);
    let host = Host {
        inbox,
        next_delivery: Arc::new(AtomicU64::new(1)),
        capacity: Arc::new(Semaphore::new(CAPACITY)),
        build: Build {
            contract: 1,
            application: config.application,
            build: config.build,
        },
        passport,
        revoked: broadcast::channel(64).0,
        operations: Arc::new(module.operations().collect()),
        connections: Arc::new(Semaphore::new(128)),
    };
    let mut router = Router::new().route("/__snap/build", get(build));
    if host.passport.is_some() {
        router = router.route("/_transport/ws", get(websocket::upgrade));
    }
    for operation in host.operations.iter().copied() {
        let path = format!("/{}", operation.key.replace('.', "/"));
        let route = match operation.lane {
            Lane::Query => get(request)
                .head(|| async { problem(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed") }),
            Lane::Submit => post(request),
            Lane::Message => continue,
        }
        .fallback(|| async { problem(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed") })
        .layer(Extension(operation));
        router = router.route(&path, route);
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
    eprintln!("listening on http://{}", listener.local_addr()?);

    // The application is moved into one host-owned loop. Request tasks only enqueue work.
    let worker = tokio::spawn(drive(module, receiver, host));
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await;
    worker.abort();
    let _ = worker.await;
    result
}

async fn shutdown() {
    #[cfg(unix)]
    if let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn drive(mut module: impl Module, mut inbox: mpsc::Receiver<Work>, host: Host) {
    let mut pending = BTreeMap::<Delivery, Pending>::new();
    let mut jobs = JoinSet::new();
    let mut actions = Vec::new();
    loop {
        let input = tokio::select! {
            work = inbox.recv() => {
                let Some(work) = work else { break };
                if work.reply.is_closed() { continue; }
                pending.insert(work.delivery, Pending { reply: work.reply, session: None, cookie: None, _permit: work.permit });
                Input::Invocation { delivery: work.delivery, invocation: work.invocation, context: work.context }
            }
            completed = jobs.join_next(), if !jobs.is_empty() => {
                let Some(Ok((delivery, result))) = completed else { continue };
                Input::Completed { delivery, result }
            }
        };
        module.update(input, &mut actions);
        for action in actions.drain(..) {
            match action {
                Action::Complete {
                    delivery,
                    operation_id: _,
                    outcome,
                } => {
                    if let Some(p) = pending.remove(&delivery) {
                        let _ = p.reply.send(Reply {
                            outcome,
                            empty: false,
                            session: p.session,
                            cookie: p.cookie,
                        });
                    }
                }
                Action::CompleteEmpty { delivery, .. } => {
                    if let Some(p) = pending.remove(&delivery) {
                        let _ = p.reply.send(Reply {
                            outcome: Ok(Value::Null),
                            empty: true,
                            session: p.session,
                            cookie: p.cookie,
                        });
                    }
                }
                Action::Work { delivery, work } => {
                    let passport = host.passport.clone();
                    jobs.spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            passport
                                .ok_or_else(|| {
                                    snap_runtime::passport::domain(
                                        "IdentityUnavailable",
                                        "Passport adapter is not configured",
                                    )
                                })?
                                .execute(work)
                        })
                        .await
                        .unwrap_or_else(|_| {
                            Err(snap_runtime::passport::domain(
                                "IdentityUnavailable",
                                "Identity work failed",
                            ))
                        });
                        (delivery, result)
                    });
                }
                Action::Session { delivery, token } => {
                    if let Some(p) = pending.get_mut(&delivery) {
                        p.cookie = Some(token);
                    }
                }
                Action::Resolved { delivery, session } => {
                    if let Some(p) = pending.get_mut(&delivery) {
                        p.session = Some(session);
                    }
                }
                Action::Revoke { sessions } => {
                    let _ = host.revoked.send(sessions);
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
    Extension(operation): Extension<Operation>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    // Match the existing host-readiness exception for Doctor.
    if operation.key != "health.up"
        && headers
            .get("x-snap-build")
            .and_then(|value| value.to_str().ok())
            != Some(host.build.build.as_str())
    {
        return problem(StatusCode::CONFLICT, "Snap Build mismatch");
    }
    let operation_id = headers
        .get("x-snap-operation-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let fields = form_urlencoded::parse(query.as_deref().unwrap_or_default().as_bytes())
        .map(|(key, value)| (key.into_owned(), Value::String(value.into_owned())))
        .collect::<serde_json::Map<String, Value>>();
    let payload = match operation.lane {
        Lane::Query => (!fields.is_empty()).then_some(Value::Object(fields)),
        Lane::Submit if body.is_empty() => None,
        Lane::Submit => match serde_json::from_slice(&body) {
            Ok(value) => Some(value),
            Err(_) => return problem(StatusCode::BAD_REQUEST, "Malformed body"),
        },
        Lane::Message => unreachable!(),
    };
    let token = if let Some(passport) = &host.passport {
        if operation.lane == Lane::Submit
            && headers
                .get("origin")
                .is_some_and(|v| v.to_str().ok() != Some(passport.origin.as_str()))
        {
            return problem(StatusCode::FORBIDDEN, "Origin not allowed");
        }
        match passport.read_cookie(&headers) {
            Ok(token) => token,
            Err(error) => return completion(operation_id, Err(error)),
        }
    } else {
        None
    };
    let invocation = Invocation {
        operation_id: operation_id.clone(),
        key: operation.key.into(),
        payload,
        traceparent: headers
            .get("traceparent")
            .and_then(|value| value.to_str().ok())
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
    if let (Some(passport), Some(token)) = (&host.passport, result.cookie)
        && let Ok(value) = passport.cookie(token.as_deref()).parse()
    {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

async fn execute(host: &Host, invocation: Invocation, token: Option<String>) -> Reply {
    let failed = |message: &str| Reply {
        outcome: Err(Error::UnavailableError {
            message: message.into(),
        }),
        empty: false,
        session: None,
        cookie: None,
    };
    let Ok(permit) = host.capacity.clone().try_acquire_owned() else {
        return failed("Host is at capacity");
    };
    let (reply, response) = oneshot::channel();
    let work = Work {
        delivery: Delivery(host.next_delivery.fetch_add(1, Ordering::Relaxed)),
        invocation,
        context: snap_runtime::passport::Context {
            token,
            now: passport::now(),
        },
        reply,
        permit,
    };
    if host.inbox.try_send(work).is_err() {
        return failed("Host is not accepting work");
    }
    match tokio::time::timeout(RESPONSE_TIMEOUT, response).await {
        Ok(Ok(response)) => response,
        _ => failed("Host did not complete the operation"),
    }
}

fn completion(operation_id: String, outcome: Outcome) -> Response {
    let status = status(&outcome);
    (
        status,
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
        Json(json!({ "error": message })),
    )
        .into_response()
}
