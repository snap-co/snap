mod keys;
mod oidc_http;
mod operations;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use snap_document_local::{
    Host,
    web::{ReadCookie, Shared},
};
use snap_identity::Identity;
use snap_store::{Error, Transaction};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tower_http::services::{ServeDir, ServeFile};

pub struct App {
    pub documents: Arc<Shared<snap_sqlite::Sqlite>>,
    pub keys: Arc<keys::Keys>,
    pub origin: String,
    pub issuer: oidc_http::Issuer,
}

impl App {
    pub fn run<T>(
        &self,
        operation: &str,
        f: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.documents.host.lock().unwrap().transact(operation, f)
    }
    pub fn bearer(&self, headers: &HeaderMap) -> Option<String> {
        self.keys.read_cookie(headers)
    }
    pub fn same_origin(&self, headers: &HeaderMap) -> bool {
        headers.get("origin").and_then(|v| v.to_str().ok()) == Some(&self.origin)
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Unix clock")
        .as_secs() as i64
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    email: String,
    password: String,
}

pub fn json_response(status: StatusCode, value: serde_json::Value) -> Response {
    (status, [("cache-control", "no-store")], Json(value)).into_response()
}

pub fn store_error(error: Error) -> Response {
    let (status, code) = match error {
        Error::NotFound => (StatusCode::UNAUTHORIZED, "invalid_credentials"),
        Error::Constraint => (StatusCode::CONFLICT, "conflict"),
        Error::Invalid => (StatusCode::BAD_REQUEST, "invalid_request"),
        Error::Miss(_) => (StatusCode::SERVICE_UNAVAILABLE, "store_miss"),
        Error::Unavailable | Error::Indeterminate => {
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        }
    };
    json_response(status, json!({"error":code}))
}

fn session_response(app: &App, account: serde_json::Value, bearer: Option<&str>) -> Response {
    let mut response = json_response(StatusCode::OK, json!({"account":account}));
    response.headers_mut().insert(
        "set-cookie",
        app.keys.cookie(bearer).parse().expect("cookie"),
    );
    response
}

async fn session(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let Some(bearer) = app.bearer(&headers) else {
        return session_response(&app, serde_json::Value::Null, None);
    };
    match app.run("authy.current", |tx| {
        authy::current(tx, &snap_crypto::Native, &bearer, now())
    }) {
        Ok(account) => json_response(StatusCode::OK, json!({"account":account})),
        Err(Error::NotFound) => session_response(&app, serde_json::Value::Null, None),
        Err(error) => store_error(error),
    }
}

async fn signup(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> Response {
    authenticate(app, headers, credentials, true)
}
async fn login(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> Response {
    authenticate(app, headers, credentials, false)
}
fn authenticate(
    app: Arc<App>,
    headers: HeaderMap,
    credentials: Credentials,
    enroll: bool,
) -> Response {
    if !app.same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let result = app.run("authy.authenticate", |tx| {
        let mut crypto = snap_crypto::Native;
        let issued = if enroll {
            authy::enroll(
                tx,
                &mut crypto,
                &credentials.email,
                &credentials.password,
                now(),
            )?
        } else {
            Identity::default().login(
                tx,
                &mut crypto,
                &credentials.email,
                &credentials.password,
                now(),
            )?
        };
        let account = authy::current(tx, &crypto, &issued.bearer, now())?;
        Ok((issued, account))
    });
    match result {
        Ok((issued, account)) => session_response(&app, json!(account), Some(&issued.bearer)),
        Err(error) => store_error(error),
    }
}
fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut migrations: Vec<_> = [
        snap_identity::MIGRATION,
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_document::server::LIFECYCLE_MIGRATION,
        authy::MIGRATION,
        snap_oidc::MIGRATION,
        keys::MIGRATION,
    ]
    .into_iter()
    .map(|source| toml::from_str(source).expect("built-in migration"))
    .collect();
    migrations.sort_by(|a: &snap_store::migration::Migration, b| a.id.cmp(&b.id));
    migrations
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database = PathBuf::from(
        std::env::var("SNAP_DATABASE").unwrap_or_else(|_| ".snap/authy-store.sqlite".into()),
    );
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--migrate"] {
        let report = snap_sqlite::migrate(&database, &migrations())?;
        println!(
            "Applied {} migrations to {}",
            report.applied.len(),
            database.display()
        );
        return Ok(());
    }
    if !args.is_empty() {
        return Err("usage: authy [--migrate]".into());
    }
    let listener = tokio::net::TcpListener::bind(
        std::env::var("AUTHY_ADDR").unwrap_or_else(|_| "127.0.0.1:3846".into()),
    )
    .await?;
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err("Authy development host requires loopback".into());
    }
    let origin = std::env::var("SNAP_ORIGIN").unwrap_or_else(|_| format!("http://{address}"));
    let parsed = url::Url::parse(&origin)?;
    if !["http", "https"].contains(&parsed.scheme())
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("SNAP_ORIGIN must be an HTTP(S) origin".into());
    }
    let origin = parsed.origin().ascii_serialization();
    let mut store = snap_sqlite::Sqlite::open(&database)?;
    for table in snap_identity::TABLES
        .iter()
        .chain(snap_access::TABLES.iter())
        .chain(snap_document::server::TABLES.iter())
        .chain(authy::TABLES.iter())
        .chain(snap_oidc::TABLES.iter())
    {
        store.load(table)?;
    }
    let keys = Arc::new(keys::Keys::load(&mut store, parsed.scheme() == "https")?);
    let host = Host::new(
        store,
        authy::document(),
        Arc::new(|tx, bearer| {
            Identity::default()
                .resolve(tx, &snap_crypto::Native, bearer, now())
                .map(|session| session.identity)
        }),
        snap_transport::server::Config::default(),
        keys::random(),
    );
    let host = operations::register(host);
    let cookie: ReadCookie = {
        let keys = keys.clone();
        Arc::new(move |headers| keys.read_cookie(headers))
    };
    let documents = Shared::with_cookie(host, origin.clone(), cookie);
    let issuer = oidc_http::Issuer::new(&origin)?;
    let app = Arc::new(App {
        documents: documents.clone(),
        keys,
        origin,
        issuer,
    });
    let assets = std::env::var("SNAP_WEB_DIR").unwrap_or_else(|_| {
        let adjacent = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("web")));
        adjacent
            .filter(|p| p.is_dir())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".snap/web".into())
    });
    let router = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/api/session", get(session))
        .route("/api/signup", post(signup))
        .route("/api/login", post(login))
        .merge(oidc_http::routes())
        .with_state(app)
        .merge(snap_document_local::web::router(documents.clone()))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .fallback_service(
            ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
        );
    println!("Authy http://{address}");
    tokio::select! {
        result=axum::serve(listener,router).with_graceful_shutdown(async {let _=tokio::signal::ctrl_c().await;})=>result?,
        _=snap_document_local::web::dispatch(documents)=>unreachable!(),
    }
    Ok(())
}
