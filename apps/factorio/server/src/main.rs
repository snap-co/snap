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
use factorio::{Workspace, workspaces as graph};
use serde_json::{Value, json};
type Host<B> = snap_transport::host::Blocking<B, snap_transport::host::Application<B>>;
use snap_identity::oauth as rp;
use snap_identity_native::oauth::{Cookies, OAuth, failure, no_store, now, random};
use snap_store::Error;
use snap_transport::native::{Prepare, Server, WebSocket, tls};
use std::{path::PathBuf, sync::Arc};
use tower_http::services::{ServeDir, ServeFile};

struct App {
    oauth: Arc<OAuth<Host<snap_store_sqlite::Sqlite>>>,
    tools: config::Tools,
    tcp: std::net::SocketAddr,
    tcp_ca_file: Option<PathBuf>,
    tcp_server_name: Option<String>,
}
impl App {
    async fn actor(&self, headers: &HeaderMap, mutation: bool) -> Result<(rp::Grant, bool), Error> {
        if let Some(header) = headers.get("authorization") {
            let bearer = header
                .to_str()
                .map_err(|_| Error::Invalid)?
                .strip_prefix("Bearer ")
                .ok_or(Error::Invalid)?;
            let bearer = bearer.to_owned();
            let credential = bearer.clone();
            let id = self
                .oauth
                .run_async("factorio.agent.select", move |tx| {
                    operations::session_id(tx, &credential).map(|(id, _)| id)
                })
                .await?;
            self.oauth.session_id(&id).await?;
            return self
                .oauth
                .run_async("factorio.agent", move |tx| operations::session(tx, &bearer))
                .await;
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
    let mut migrations: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_store::resource::MIGRATION,
        snap_identity::MIGRATION,
        snap_identity_native::oauth::MIGRATION,
        factorio::MIGRATION,
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    migrations
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(options) = factorio_cli::dispatch().await? {
        serve(options).await?;
    }
    Ok(())
}
async fn serve(options: snap_config::Options) -> Result<(), Box<dyn std::error::Error>> {
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
        snap_store_sqlite::migrate(&database, &migrations())?;
        println!("Factorio migrations applied");
        return Ok(());
    }
    let secrets = startup.load_secrets()?;
    // Check-config is schema-only for deployment packaging. Serving loads and
    // validates mounted TLS material before either listener can bind.
    let tcp_tls = tls::ServerTls::new(
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
    tls::ClientTls::new(
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
    let origin = snap_identity_native::oauth::origin(&startup.host.public_origin(address))?;
    let mut store = snap_store_sqlite::Sqlite::open(&database)?;
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(core::iter::once(&snap_store::resource::TABLE))
        .chain(rp::TABLES.iter())
        .chain(["factorio.agents"].iter())
        .chain(["factorio.cli"].iter())
        .chain(["factorio.cli_login"].iter())
    {
        store.load(table)?;
    }
    let cookies = Cookies::load(&mut store, "factorio", origin.starts_with("https:"))?;
    let document = Arc::new(graph::document());
    let mut registry = snap_transport::operation::Registry::default();
    for definition in snap_document::operations::definitions(document.clone()) {
        registry = registry.with_request(definition);
    }
    let registry = operations::register(registry, config, origin.clone());
    let host = Host::new(
        store,
        snap_transport::host::Application::new(vec![snap_document::sync::binding(document)]),
        registry,
        Arc::new(snap_transport::bearer::Callbacks::with_retained(
            Arc::new(|tx, bearer| operations::session(tx, bearer).map(|(s, _)| s.owner)),
            Arc::new(operations::retained),
        )),
        snap_transport::server::Config {
            reconnect_ms: startup.app.tcp.retention_ms,
            ..Default::default()
        },
        random(),
    )
    .with_inputs(operations::inputs);
    let host = controller::register(host, tokio::runtime::Handle::current(), tools.clone());
    let transport = Server::new(host).await?;
    let websocket = transport.websocket(WebSocket {
        origin: origin.clone(),
        cookie: Some(cookies.reader()),
        require_cookie: false,
    });
    let oauth = OAuth::new(
        transport.transactions(),
        cookies,
        snap_identity_native::oauth::Config {
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
        .merge(websocket)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .fallback_service(
            ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
        );
    println!("Factorio http://{address}");
    println!("Factorio tls://{tcp_address}");
    let prepare: Prepare = Arc::new(move |command| {
        let oauth = oauth.clone();
        Box::pin(async move {
            login::prepare(&oauth, command)
                .await
                .map_err(|error| match error {
                    Error::Unavailable => snap_transport::Error::Unavailable,
                    _ => snap_transport::Error::InvalidBearer,
                })
        })
    });
    transport
        .run(
            tcp_listener,
            tcp_tls,
            Some(prepare),
            axum::serve(listener, router).with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            }),
        )
        .await?;
    Ok(())
}
