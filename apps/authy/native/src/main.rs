mod config;
mod keys;
mod oidc_http;
mod operations;
mod pages;

use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;
use snap_document_local::{
    Host,
    web::{ReadCookie, Shared},
};
use snap_identity::Identity;
use snap_store::{Error, Transaction};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tower_http::services::{ServeDir, ServeFile};

pub struct App {
    pub documents: Arc<Shared<snap_sqlite::Sqlite>>,
    pub keys: Arc<keys::Keys>,
    pub origin: String,
    pub issuer: oidc_http::Issuer,
    pub pages: pages::Pages,
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
    let options = snap_config::Options::parse()?;
    let config = snap_config::Config::<config::Settings>::read(&options.config)?;
    config.app.validate()?;
    if options.action == snap_config::Action::Check {
        config.require_bag(
            config.app.cookie_key_ref.is_some()
                || config
                    .app
                    .clients
                    .iter()
                    .any(|c| c.client_secret_ref.is_some()),
        )?;
    }
    if options.action == snap_config::Action::Check {
        return Ok(());
    }
    let database = config.database();
    if options.action == snap_config::Action::Migrate {
        std::fs::create_dir_all(database.parent().unwrap())?;
        let report = snap_sqlite::migrate(&database, &migrations())?;
        println!(
            "Applied {} migrations to {}",
            report.applied.len(),
            database.display()
        );
        return Ok(());
    }
    let secrets = config.load_secrets()?;
    let cookie_key = config.app.cookie_key(&secrets)?;
    if cookie_key
        .as_ref()
        .is_some_and(|key| key.expose().len() < 32)
    {
        return Err("Cookie key must contain at least 32 bytes".into());
    }
    // Resolve required issuer secrets before opening a listener or mutating Store.
    let mut issuer = oidc_http::Issuer::new(
        &config.host.public_origin(config.host.listen),
        &config.app,
        &secrets,
        &config.host.dev_client_origins,
    )?;
    let listener = tokio::net::TcpListener::bind(config.host.listen).await?;
    let address = listener.local_addr()?;
    let origin = config.host.public_origin(address);
    let parsed = url::Url::parse(&origin)?;
    if !["http", "https"].contains(&parsed.scheme())
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("host.origin must be an HTTP(S) origin".into());
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
    let keys = Arc::new(keys::Keys::load(
        &mut store,
        parsed.scheme() == "https",
        cookie_key.as_ref(),
    )?);
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
    let documents = Shared::with_required_cookie(host, origin.clone(), cookie);
    issuer.config.issuer = origin.clone();
    let assets = config.assets().to_string_lossy().into_owned();
    let app = Arc::new(App {
        documents: documents.clone(),
        keys,
        origin,
        issuer,
        pages: pages::Pages::load(&assets)?,
    });
    let identity_routes = snap_document_local::web::http_router(
        documents.clone(),
        vec![
            snap_document_local::web::HttpOperation {
                name: "identity.acquire",
                method: Method::POST,
                session: snap_document_local::web::SessionProjection::Issue,
            },
            snap_document_local::web::HttpOperation {
                name: "identity.enroll",
                method: Method::POST,
                session: snap_document_local::web::SessionProjection::Issue,
            },
            snap_document_local::web::HttpOperation {
                name: "identity.fetch",
                method: Method::GET,
                session: snap_document_local::web::SessionProjection::Fetch,
            },
        ],
        {
            let keys = app.keys.clone();
            Arc::new(move |bearer| keys.cookie(bearer))
        },
    );
    let router = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route(
            "/api/{*path}",
            axum::routing::any(|| async { StatusCode::NOT_FOUND }),
        )
        .merge(oidc_http::routes(app.clone()))
        .with_state(app)
        .merge(identity_routes)
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
