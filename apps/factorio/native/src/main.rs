mod controller;
mod effects;
mod intake;
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
use snap_oauth_local::{Config, Cookies, OAuth, failure, no_store, now, random};
use snap_oidc::relying_party as rp;
use snap_store::Error;
use std::{path::PathBuf, sync::Arc};
use tower_http::services::{ServeDir, ServeFile};

struct App {
    oauth: Arc<OAuth>,
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
fn env(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.into())
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
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    migrations
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database = PathBuf::from(env("SNAP_DATABASE", ".snap/factorio-store.sqlite"));
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--migrate"] {
        snap_sqlite::migrate(&database, &migrations())?;
        println!("Factorio migrations applied");
        return Ok(());
    }
    if !args.is_empty() {
        return Err("usage: factorio [--migrate]".into());
    }
    let mut config: factorio::Config =
        serde_json::from_slice(&std::fs::read(env("FACTORIO_CONFIG", "factorio.json"))?)?;
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
    let listener = tokio::net::TcpListener::bind(env("FACTORIO_ADDR", "127.0.0.1:3852")).await?;
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err("Factorio requires loopback".into());
    }
    let origin = snap_oauth_local::origin(&env("SNAP_ORIGIN", &format!("http://{address}")))?;
    let mut store = snap_sqlite::Sqlite::open(&database)?;
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
        .chain(["factorio.agents"].iter())
    {
        store.load(table)?;
    }
    let cookies = Cookies::load(&mut store, "factorio", origin.starts_with("https:"))?;
    let host = Host::new(
        store,
        graph::document(),
        Arc::new(|tx, bearer| operations::session(tx, bearer).map(|(s, _)| s.owner)),
        snap_transport::server::Config::default(),
        random(),
    );
    let host = operations::register(host, config);
    let mut host = controller::register(host, tokio::runtime::Handle::current());
    tokio::task::block_in_place(|| host.recover_controllers())?;
    let documents = Shared::with_cookie(host, origin.clone(), cookies.reader());
    let oauth = OAuth::new(
        documents.clone(),
        cookies,
        Config {
            origin,
            issuer: env("AUTHY_ORIGIN", "http://127.0.0.1:3846"),
            client: "factorio".into(),
            secret: env("FACTORIO_CLIENT_SECRET", ""),
        },
    )?;
    let app = Arc::new(App {
        oauth: oauth.clone(),
    });
    let assets = std::env::var("SNAP_WEB_DIR").unwrap_or_else(|_| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("web")))
            .filter(|p| p.is_dir())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "apps/factorio/.snap/web".into())
    });
    let router = Router::new()
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
    tokio::select! { result=axum::serve(listener,router).with_graceful_shutdown(async {let _=tokio::signal::ctrl_c().await;}) => result?, _=snap_document_local::web::dispatch(documents)=>unreachable!() }
    Ok(())
}
