//! Native execution and HTTP IO. All Tokio, Axum, OS, and environment access lives here.

pub mod client;

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
    extract::{RawQuery, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use snap_protocol::{Completion, Error, Invocation, Lane, Operation, Outcome, Value, json};
use snap_runtime::{Action, Delivery, Input, Module};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, mpsc, oneshot},
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
    reply: oneshot::Sender<Response>,
}

#[derive(Clone)]
struct Host {
    inbox: mpsc::Sender<Work>,
    next_delivery: Arc<AtomicU64>,
    capacity: Arc<Semaphore>,
    build: Build,
}

pub fn run(module: impl Module + Send + 'static, config: Config) -> io::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(module, config))
}

async fn serve(module: impl Module + Send + 'static, config: Config) -> io::Result<()> {
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
    };
    let mut router = Router::new().route("/__snap/build", get(build));
    for operation in module.operations() {
        let path = format!("/{}", operation.key.replace('.', "/"));
        let route = match operation.lane {
            Lane::Query => get(query)
                .head(|| async { problem(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed") }),
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
    .with_state(host);
    let listener = TcpListener::bind(config.address).await?;
    eprintln!("listening on http://{}", listener.local_addr()?);

    // The application is moved into one host-owned loop. Request tasks only enqueue work.
    let worker = tokio::spawn(drive(module, receiver));
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

async fn drive(mut module: impl Module, mut inbox: mpsc::Receiver<Work>) {
    let mut pending = BTreeMap::<Delivery, oneshot::Sender<Response>>::new();
    let mut actions = Vec::new();
    while let Some(work) = inbox.recv().await {
        // Dropped/timed-out HTTP observers do not retain response slots indefinitely.
        pending.retain(|_, reply| !reply.is_closed());
        if work.reply.is_closed() {
            continue;
        }
        pending.insert(work.delivery, work.reply);
        module.update(
            Input::Invocation {
                delivery: work.delivery,
                invocation: work.invocation,
            },
            &mut actions,
        );
        for action in actions.drain(..) {
            match action {
                Action::Complete {
                    delivery,
                    operation_id,
                    outcome,
                } => {
                    if let Some(reply) = pending.remove(&delivery) {
                        let _ = reply.send(completion(operation_id, outcome));
                    }
                }
            }
        }
    }
}

async fn build(State(host): State<Host>) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(host.build)).into_response()
}

async fn query(
    State(host): State<Host>,
    Extension(operation): Extension<Operation>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
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
    let _permit = match host.capacity.try_acquire() {
        Ok(permit) => permit,
        Err(_) => return unavailable(operation_id, "Host is at capacity"),
    };
    let fields = form_urlencoded::parse(query.as_deref().unwrap_or_default().as_bytes())
        .map(|(key, value)| (key.into_owned(), Value::String(value.into_owned())))
        .collect::<serde_json::Map<String, Value>>();
    let payload = (!fields.is_empty()).then_some(Value::Object(fields));
    let invocation = Invocation {
        operation_id: operation_id.clone(),
        key: operation.key.into(),
        payload,
        traceparent: headers
            .get("traceparent")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    };
    let (reply, response) = oneshot::channel();
    let work = Work {
        delivery: Delivery(host.next_delivery.fetch_add(1, Ordering::Relaxed)),
        invocation,
        reply,
    };
    if host.inbox.try_send(work).is_err() {
        return unavailable(operation_id, "Host is not accepting work");
    }
    match tokio::time::timeout(RESPONSE_TIMEOUT, response).await {
        Ok(Ok(response)) => response,
        _ => unavailable(operation_id, "Host did not complete the query"),
    }
}

fn completion(operation_id: String, outcome: Outcome) -> Response {
    let status = match &outcome {
        Ok(_) => StatusCode::OK,
        Err(Error::ContractViolationError { message }) if message.starts_with("Unknown key:") => {
            StatusCode::NOT_FOUND
        }
        Err(Error::InvalidInputError { .. } | Error::ContractViolationError { .. }) => {
            StatusCode::BAD_REQUEST
        }
        Err(Error::UnavailableError { .. }) => StatusCode::SERVICE_UNAVAILABLE,
    };
    (
        status,
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(Completion::new(operation_id, outcome)),
    )
        .into_response()
}

fn unavailable(operation_id: String, message: &str) -> Response {
    completion(
        operation_id,
        Err(Error::UnavailableError {
            message: message.into(),
        }),
    )
}

fn problem(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "error": message })),
    )
        .into_response()
}
