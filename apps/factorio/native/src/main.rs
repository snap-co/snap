mod config;
mod controller;
mod effects;
mod intake;
mod login;
mod operations;
#[cfg(test)]
mod tests;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use factorio::{Workspace, documents as graph};
use serde_json::{Value, json};
use snap_document_local::{Host, web::Shared};
use snap_oauth_local::{Cookies, OAuth, failure, no_store, now, random};
use snap_oidc::relying_party as rp;
use snap_store::Error;
use std::{path::PathBuf, sync::Arc};
use tower_http::services::{ServeDir, ServeFile};

struct App {
    oauth: Arc<OAuth>,
    tools: config::Tools,
    tcp: std::net::SocketAddr,
    tcp_ca_file: Option<PathBuf>,
    tcp_server_name: Option<String>,
}
impl App {
    async fn actor(
        &self,
        headers: &HeaderMap,
        mutation: bool,
    ) -> Result<(rp::Session, bool), Error> {
        if let Some(header) = headers.get("authorization") {
            let bearer = header
                .to_str()
                .map_err(|_| Error::Invalid)?
                .strip_prefix("Bearer ")
                .ok_or(Error::Invalid)?;
            return self
                .oauth
                .run("factorio.agent", |tx| operations::session(tx, bearer));
        }
        let s = self.oauth.session(headers).await?;
        if mutation {
            self.oauth.csrf(headers, &s)?;
        }
        Ok((s, true))
    }
}
async fn session(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    match app.actor(&headers, false).await {
        Ok((s, human)) => {
            no_store(json!({"identified":true,"csrf":s.csrf,"owner":s.owner,"human":human}))
        }
        Err(Error::NotFound) => no_store(json!({"identified":false})),
        Err(e) => failure(e),
    }
}
fn migrations() -> Vec<snap_store::migration::Migration> {
    // Historical migrations stay immutable. Intake keys are no longer used.
    let mut migrations: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_document::server::LIFECYCLE_MIGRATION,
        rp::MIGRATION,
        snap_oauth_local::MIGRATION,
        include_str!("../migrations/0003_factorio_agents.toml"),
        include_str!("../migrations/0004_factorio_intake.toml"),
        include_str!("../migrations/0005_factorio_cli.toml"),
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    migrations
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = snap_config::Options::parse()?;
    let startup = snap_config::Config::<config::Settings>::read(&options.config)?;
    startup.app.validate()?;
    if options.action == snap_config::Action::Check {
        startup.require_bag(true)?;
    }
    if options.action == snap_config::Action::Check {
        return Ok(());
    }
    let database = startup.database();
    if options.action == snap_config::Action::Migrate {
        std::fs::create_dir_all(database.parent().unwrap())?;
        snap_sqlite::migrate(&database, &migrations())?;
        println!("Factorio migrations applied");
        return Ok(());
    }
    let secrets = startup.load_secrets()?;
    // Check-config is schema-only for deployment packaging. Serving loads and
    // validates mounted TLS material before either listener can bind.
    let tcp_tls = snap_transport_native::tls::ServerTls::new(
        &startup.path(&startup.app.tcp.cert_file),
        &startup.path(&startup.app.tcp.key_file),
    )?;
    let tcp_ca_file = startup
        .app
        .tcp
        .ca_file
        .as_deref()
        .map(|path| startup.path(path));
    let tcp_ca_file = tcp_ca_file.map(std::fs::canonicalize).transpose()?;
    snap_transport_native::tls::ClientTls::new(
        tcp_ca_file.as_deref(),
        startup.app.tcp.server_name.as_deref(),
    )?;
    let oauth_config = startup.app.oauth.resolve(
        startup.host.public_origin(startup.host.listen),
        &secrets,
        startup.host.dev_origins.clone(),
    )?;
    let mut tools = startup.app.tools.clone();
    tools.bridge = Some(
        startup.path(
            tools
                .bridge
                .as_deref()
                .unwrap_or(std::path::Path::new("bridge.js")),
        ),
    );
    let mut config = startup.app.repository.clone();
    for path in [&config.repository, &config.resources] {
        if !PathBuf::from(path).is_absolute() {
            return Err("Repository and resources must be absolute paths".into());
        }
    }
    std::fs::create_dir_all(&config.resources)?;
    config.repository = std::fs::canonicalize(&config.repository)?
        .to_str()
        .ok_or("Repository must be UTF-8")?
        .into();
    config.resources = std::fs::canonicalize(&config.resources)?
        .to_str()
        .ok_or("Resources must be UTF-8")?
        .into();
    if PathBuf::from(&config.resources).starts_with(&config.repository) {
        return Err("Resources must live outside the managed repository".into());
    }
    if config.modules.is_empty()
        || config.modules.iter().any(|(name, path)| {
            name == "*"
                || path.is_empty()
                || PathBuf::from(path).is_absolute()
                || path.split('/').any(|p| matches!(p, ".." | "." | ""))
        })
    {
        return Err("Modules must name repository-relative crate directories".into());
    }
    for (name, path) in &config.modules {
        if config.modules.iter().any(|(other, dir)| {
            other != name
                && (PathBuf::from(path).starts_with(dir) || PathBuf::from(dir).starts_with(path))
        }) {
            return Err("Module directories must not overlap".into());
        }
        if std::fs::canonicalize(PathBuf::from(&config.repository).join(path))?
            != PathBuf::from(&config.repository).join(path)
        {
            return Err("Module directories must not contain symlinks".into());
        }
    }
    effects::git(
        &config.repository,
        &[
            "check-ref-format",
            &format!("refs/heads/{}", config.mainline),
        ],
    )
    .await?;
    effects::head(&config).await?;
    let listener = tokio::net::TcpListener::bind(startup.host.listen).await?;
    let address = listener.local_addr()?;
    let tcp_listener = tokio::net::TcpListener::bind(startup.app.tcp.listen).await?;
    let tcp_address = tcp_listener.local_addr()?;
    let origin = snap_oauth_local::origin(&startup.host.public_origin(address))?;
    let mut store = snap_sqlite::Sqlite::open(&database)?;
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
        .chain(["factorio.agents"].iter())
        .chain(["factorio.cli"].iter())
        .chain(["factorio.cli_login"].iter())
    {
        store.load(table)?;
    }
    let cookies = Cookies::load(&mut store, "factorio", origin.starts_with("https:"))?;
    let host = Host::new(
        store,
        graph::document(),
        Arc::new(|tx, bearer| operations::session(tx, bearer).map(|(s, _)| s.owner)),
        snap_transport::server::Config {
            reconnect_ms: startup.app.tcp.retention_ms,
            ..Default::default()
        },
        random(),
    );
    let host = operations::register(host, config, origin.clone());
    let mut host = controller::register(host, tokio::runtime::Handle::current(), tools.clone());
    tokio::task::block_in_place(|| host.recover_controllers())?;
    let documents = Shared::with_cookie(host, origin.clone(), cookies.reader());
    let oauth = OAuth::new(
        documents.clone(),
        cookies,
        snap_oauth_local::Config {
            origin,
            ..oauth_config
        },
    )?;
    let app = Arc::new(App {
        oauth: oauth.clone(),
        tools,
        tcp: std::net::SocketAddr::new(
            if tcp_address.ip().is_unspecified() {
                if tcp_address.is_ipv4() {
                    std::net::Ipv4Addr::LOCALHOST.into()
                } else {
                    std::net::Ipv6Addr::LOCALHOST.into()
                }
            } else {
                tcp_address.ip()
            },
            tcp_address.port(),
        ),
        tcp_ca_file,
        tcp_server_name: startup.app.tcp.server_name.clone(),
    });
    let assets = startup.assets().to_string_lossy().into_owned();
    let router = Router::new()
        .route("/auth/cli/{code}", get(login::page).post(login::approve))
        .route("/health", get(|| async { "OK" }))
        .route("/api/session", get(session))
        // OpenCode calls stay outside the gate: its agents can call back over WS.
        .route(
            "/api/workspaces/{workspace}/intakes/{id}/opencode",
            post(intake::action),
        )
        .route(
            "/api/workspaces/{workspace}/intakes/{id}/events",
            get(intake::events),
        )
        .with_state(app)
        .merge(oauth.routes())
        .merge(snap_document_local::web::router(documents.clone()))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .fallback_service(
            ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
        );
    println!("Factorio http://{address}");
    println!("Factorio tls://{tcp_address}");
    tokio::select! { result=axum::serve(listener,router).with_graceful_shutdown(async {let _=tokio::signal::ctrl_c().await;}) => result?, result=snap_document_local::tcp::serve(tcp_listener,documents.clone(),tcp_tls)=>result?, _=snap_document_local::web::dispatch(documents)=>unreachable!() }
    Ok(())
}
