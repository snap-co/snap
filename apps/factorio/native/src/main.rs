mod effects;
mod intake;
#[cfg(test)]
mod tests;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use factorio::{Actor, Command, Effect, Phase, Workspace};
use serde_json::{Value, json};
use snap_document_local::{Host, web::Shared};
use snap_oauth_local::{Config, Cookies, OAuth, failure, no_store, now, random};
use snap_oidc::relying_party as rp;
use snap_store::Error;
use std::{path::PathBuf, sync::Arc};
use tower_http::services::{ServeDir, ServeFile};

struct App {
    oauth: Arc<OAuth>,
    effects: tokio::sync::Mutex<()>,
    intake_gate: tokio::sync::Mutex<()>,
}
impl App {
    fn workspace(&self) -> Result<Workspace, Error> {
        self.oauth.run("factorio.inspect", factorio::load)
    }
    fn effect(&self, id: &str, effect: Effect) -> Result<Workspace, String> {
        self.oauth
            .run("factorio.effect", |tx| factorio::effect(tx, id, effect))
            .map_err(|e| format!("{e:?}"))
    }
    fn intent(&self, session: &rp::Session, id: &str, effect: Effect) -> Result<Workspace, String> {
        self.oauth
            .run("factorio.authorized-intent", |tx| {
                factorio::authorized_intent(
                    tx,
                    Actor {
                        session: &session.id,
                        human: false,
                        now: now(),
                    },
                    id,
                    effect,
                )
            })
            .map_err(|e| format!("{e:?}"))
    }
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
            let s = self.oauth.run("factorio.agent", |tx| {
                let row = tx
                    .get("factorio.agents", &[rp::digest(bearer).into()])?
                    .ok_or(Error::NotFound)?;
                let Some(snap_store::Value::Text(session)) = row.get("session") else {
                    return Err(Error::Invalid);
                };
                rp::lease(tx, session, now())
            })?;
            return Ok((s, false));
        }
        let s = self.oauth.session(headers).await?;
        if mutation {
            self.oauth.csrf(headers, &s)?;
        }
        Ok((s, true))
    }
    async fn recover(&self, id: &str) -> Result<(), String> {
        let w = self.workspace().map_err(|e| format!("{e:?}"))?;
        let s = w.sessions.get(id).ok_or("Session not found")?;
        match s.phase {
            Phase::Starting => {
                effects::setup(&w.config, s).await?;
                self.effect(id, Effect::Started)?;
            }
            Phase::Integrating => {
                effects::integrate(&w.config, s).await?;
                self.effect(id, Effect::Integrated)?;
            }
            _ => {}
        }
        let w = self.workspace().map_err(|e| format!("{e:?}"))?;
        let s = w.sessions.get(id).ok_or("Session not found")?;
        if matches!(s.phase, Phase::Cleanup | Phase::Abandoning) {
            effects::cleanup(&w.config, s).await?;
            self.effect(id, Effect::Cleaned)?;
        }
        Ok(())
    }
    async fn execute(&self, session: &rp::Session, mut input: Value) -> Result<Workspace, String> {
        let _gate = self.effects.lock().await;
        let name = input["command"]
            .as_str()
            .ok_or("Missing command")?
            .to_owned();
        let id = input["id"].as_str().unwrap_or("").to_owned();
        let w = self.workspace().map_err(|e| format!("{e:?}"))?;
        // Recheck local OAuth authority after waiting for external-effect exclusion.
        self.oauth
            .run("factorio.authority", |tx| rp::lease(tx, &session.id, now()))
            .map_err(|_| "Session expired")?;
        let result: Result<(), String> = async {
            match name.as_str() {
                "publish" => {
                    let s = w.sessions.get(&id).ok_or("Session not found")?;
                    let evidence = input["evidence"]
                        .as_str()
                        .ok_or("Provide check/review evidence")?
                        .to_owned();
                    let findings =
                        serde_json::from_value(input.get("findings").cloned().unwrap_or(json!([])))
                            .map_err(|_| "Invalid findings")?;
                    let (commit, target) = effects::candidate(&w.config, s).await?;
                    self.intent(
                        session,
                        &id,
                        Effect::Published {
                            commit,
                            target,
                            evidence,
                            findings,
                        },
                    )?;
                }
                "accept" => {
                    let s = w.sessions.get(&id).ok_or("Session not found")?;
                    if s.phase == Phase::Published {
                        if s.candidate.as_ref().is_none_or(|c| c.approval.is_none()) {
                            return Err("Awaiting explicit human approval in the browser".into());
                        }
                        let commit = effects::prepare(&w.config, s).await?;
                        self.intent(session, &id, Effect::Integrating { commit })?;
                    } else if !matches!(s.phase, Phase::Integrating | Phase::Cleanup) {
                        return Err("Session cannot be accepted in this phase".into());
                    }
                    self.recover(&id).await?;
                }
                "recover" | "cleanup" => {
                    self.recover(&id).await?;
                }
                "approve" => return Err("Approval requires the human browser action".into()),
                _ => {
                    if name == "start" {
                        input["base"] = effects::head(&w.config).await?.into();
                        if input.get("conversation").is_none() {
                            input["conversation"] =
                                format!("ses_{}", uuid::Uuid::new_v4().simple()).into();
                        }
                    }
                    let command: Command =
                        serde_json::from_value(input).map_err(|e| e.to_string())?;
                    self.oauth
                        .run("factorio.command", |tx| {
                            factorio::command(
                                tx,
                                Actor {
                                    session: &session.id,
                                    human: false,
                                    now: now(),
                                },
                                command,
                            )
                        })
                        .map_err(|e| format!("{e:?}"))?;
                    if name == "start" || name == "abandon" {
                        self.recover(&id).await?;
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if !id.is_empty() {
                let _ = self.effect(&id, Effect::Failed(error.clone()));
            }
            return Err(error);
        }
        self.workspace().map_err(|e| format!("{e:?}"))
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
async fn workspace(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let (s, _) = match app.actor(&headers, false).await {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    match app.oauth.run("factorio.view", |tx| {
        rp::lease(tx, &s.id, now())?;
        factorio::load(tx)
    }) {
        Ok(w) => no_store(json!(w)),
        Err(e) => failure(e),
    }
}
async fn command(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    let (s, _) = match app.actor(&headers, true).await {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    match app.execute(&s, input).await {
        Ok(w) => no_store(json!(w)),
        Err(e) => (StatusCode::CONFLICT, Json(json!({"error_description":e}))).into_response(),
    }
}
async fn approve(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    let (s, human) = match app.actor(&headers, true).await {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    if !human {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(id) = input["id"].as_str() else {
        return failure(Error::Invalid);
    };
    let Some(commit) = input["commit"].as_str() else {
        return failure(Error::Invalid);
    };
    let _gate = app.effects.lock().await;
    let result = app.oauth.run("factorio.human-approval", |tx| {
        factorio::command(
            tx,
            Actor {
                session: &s.id,
                human: true,
                now: now(),
            },
            Command::Approve {
                id: id.into(),
                commit: commit.into(),
            },
        )
    });
    match result {
        Ok(w) => no_store(json!(w)),
        Err(e) => failure(e),
    }
}
async fn token(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let (s, human) = match app.actor(&headers, true).await {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    if !human {
        return StatusCode::FORBIDDEN.into_response();
    }
    let token = random();
    match app.oauth.run("factorio.agent-token", |tx| {
        rp::lease(tx, &s.id, now())?;
        tx.insert(
            "factorio.agents",
            [
                ("id".into(), rp::digest(&token).into()),
                ("session".into(), s.id.clone().into()),
            ]
            .into_iter()
            .collect(),
        )
    }) {
        Ok(()) => no_store(json!({"token":token})),
        Err(e) => failure(e),
    }
}
fn env(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.into())
}
fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut migrations: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
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
    if config.modules.iter().any(|(name, path)| {
        name == "*"
            || path.is_empty()
            || PathBuf::from(path).is_absolute()
            || path.split('/').any(|p| matches!(p, ".." | "." | ""))
    }) {
        return Err("Modules must name repository-relative crate directories".into());
    }
    for (name, path) in &config.modules {
        if config.modules.iter().any(|(other, dir)| {
            other != name
                && (PathBuf::from(path).starts_with(dir) || PathBuf::from(dir).starts_with(path))
        }) {
            return Err("Module directories must not overlap".into());
        }
        let resolved = std::fs::canonicalize(PathBuf::from(&config.repository).join(path))?;
        if resolved != PathBuf::from(&config.repository).join(path) {
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
        .chain(["factorio.agents", "factorio.intake_keys"].iter())
    {
        store.load(table)?;
    }
    store.run("factorio.initialize", |tx| {
        factorio::initialize(tx, &config)
    })?;
    let cookies = Cookies::load(&mut store, "factorio", origin.starts_with("https:"))?;
    let host = Host::new(
        store,
        factorio::document(),
        Arc::new(|tx, bearer| rp::lease(tx, &rp::digest(bearer), now()).map(|s| s.owner)),
        snap_transport::server::Config::default(),
        random(),
    );
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
        effects: tokio::sync::Mutex::new(()),
        intake_gate: tokio::sync::Mutex::new(()),
    });
    // Only reconcile committed integration/cleanup at boot. Interrupted setup is
    // visible and requires explicit recovery before any hook is run again.
    for s in app.workspace()?.sessions.values() {
        effects::reap_hook(s)?;
        if matches!(
            s.phase,
            Phase::Integrating | Phase::Cleanup | Phase::Abandoning
        ) {
            if let Err(e) = app.recover(&s.id).await {
                let _ = app.effect(&s.id, Effect::Failed(e));
            }
        } else if s.phase == Phase::Starting {
            app.effect(
                &s.id,
                Effect::Failed("Setup interrupted; inspect resources and run recover".into()),
            )?;
        }
    }
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
        .route("/api/workspace", get(workspace))
        .route("/api/command", post(command))
        .route("/api/approve", post(approve))
        .route("/api/token", post(token))
        .route("/api/intakes", post(intake::create))
        .route("/api/intakes/{id}", post(intake::action))
        .route("/api/intakes/{id}/events", get(intake::events))
        .route("/api/intake-tool", post(intake::tool))
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
