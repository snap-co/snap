use axum::{Router, extract::State, http::HeaderMap, response::Response, routing::get};
use serde_json::{Value, json};
use snap_document_local::{Host, web::Shared};
use snap_oauth_local::{Cookies, OAuth, failure, no_store, now, random};
use snap_oidc::relying_party as rp;
use snap_store::Error;
use snap_transport::operation::{Definition as Request, Guard};
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};

async fn session(State(oauth): State<Arc<OAuth>>, headers: HeaderMap) -> Response {
    match oauth.session(&headers).await {
        Ok(session) => no_store(
            json!({"identified":true,"csrf":session.csrf,"account":{"id":session.subject,"owner":session.owner,"name":session.profile["name"],"email":session.profile["email"]}}),
        ),
        Err(Error::NotFound) => no_store(json!({"identified":false})),
        Err(error) => failure(error),
    }
}

fn field<'a>(input: &'a Value, name: &str) -> Result<&'a str, Error> {
    input[name].as_str().ok_or(Error::Invalid)
}

fn operations(mut host: Host<snap_sqlite::Sqlite>) -> Host<snap_sqlite::Sqlite> {
    host = host.with_request(Request {
        name: "chatty.create".into(),
        identity_required: true,
        input: |v| serde_json::from_value::<chatty::Create>(v.clone()).is_ok(),
        output: Value::is_object,
        progress: |_| false,
        error: |_| true,
        guards: vec![],
        inputs: &[],
        tables: &[],
        handler: snap_transport::operation::Handler::new(|tx, invocation, owner, _, _| {
            let input = serde_json::from_value::<chatty::Create>(invocation.input.clone())
                .map_err(|_| Error::Invalid)?;
            chatty::create(tx, owner.ok_or(Error::Invalid)?, &input)?;
            Ok(json!({"id": input.id}))
        }),
    });
    for name in ["chatty.send", "chatty.rename", "chatty.delete"] {
        host = host.with_request(Request {
            name: name.into(), identity_required: true, input: |v| v["thread_id"].is_string(), output: Value::is_object, progress: |_| false,
            tables: &[],
            inputs: &[], error: |_| true, guards: vec![Guard::policy(|tx, actor, input, _| {
                let snapshot = chatty::document().read(tx, field(input, "thread_id")?, actor)?;
                let resource = snap_access::Resource::new("document", &snapshot.id).map_err(|_| Error::Invalid)?;
                if !snap_access::allows(snap_document::access::vocabulary().role(tx, &resource, actor, false)?, snap_access::Role::Owner) { return Err(Error::Invalid); }
                Ok(())
            })],
            handler: snap_transport::operation::Handler::new(move |tx, invocation, owner, _, _| {
                let owner = owner.ok_or(Error::Invalid)?;
                let input = &invocation.input;
                let id = field(input, "thread_id")?;
                let mutation = match name { "chatty.send" => "send", "chatty.rename" => "rename", _ => "document.delete" };
                let args = match name {
                    "chatty.send" => json!({"id":field(input,"request_id")?,"message":field(input,"message")?,"created":now()}),
                    "chatty.rename" => json!({"title":field(input,"title")?}),
                    _ => Value::Null,
                };
                chatty::mutate(tx, owner, id, mutation, args)?;
                Ok(json!({"saved":true}))
            }),
        });
    }
    host
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    oauth: snap_oauth_local::Settings,
}
fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut values: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_document::server::LIFECYCLE_MIGRATION,
        rp::MIGRATION,
        snap_oauth_local::MIGRATION,
        chatty::MIGRATION,
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    values.sort_by(|a, b| a.id.cmp(&b.id));
    values
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = snap_config::Options::parse()?;
    let config = snap_config::Config::<Settings>::read(&options.config)?;
    config.app.oauth.validate()?;
    if options.action == snap_config::Action::Check {
        config.require_bag(true)?;
    }
    if options.action == snap_config::Action::Check {
        return Ok(());
    }
    let database = config.database();
    if options.action == snap_config::Action::Migrate {
        std::fs::create_dir_all(database.parent().unwrap())?;
        snap_sqlite::migrate(&database, &migrations())?;
        println!("Chatty migrations applied");
        return Ok(());
    }
    let secrets = config.load_secrets()?;
    let oauth_config = config.app.oauth.resolve(
        config.host.public_origin(config.host.listen),
        &secrets,
        config.host.dev_origins.clone(),
    )?;
    let listener = tokio::net::TcpListener::bind(config.host.listen).await?;
    let address = listener.local_addr()?;
    let origin = snap_oauth_local::origin(&config.host.public_origin(address))?;
    let mut store = snap_sqlite::Sqlite::open(&database)?;
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
        .chain(chatty::TABLES.iter())
    {
        store.load(table)?;
    }
    let cookies = Cookies::load(&mut store, "chatty", origin.starts_with("https:"))?;
    let host = operations(Host::new(
        store,
        chatty::document(),
        Arc::new(|tx, bearer| rp::lease(tx, &rp::digest(bearer), now()).map(|s| s.owner)),
        snap_transport::server::Config::default(),
        random(),
    ));
    let documents = Shared::with_cookie(host, origin.clone(), cookies.reader());
    let oauth = OAuth::new(
        documents.clone(),
        cookies,
        snap_oauth_local::Config {
            origin,
            ..oauth_config
        },
    )?;
    let assets = config.assets().to_string_lossy().into_owned();
    let router = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/api/session", get(session))
        .with_state(oauth.clone())
        .merge(oauth.routes())
        .merge(snap_document_local::web::router(documents.clone()))
        .fallback_service(
            ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
        );
    println!("Chatty http://{address}");
    tokio::select! {result=axum::serve(listener,router).with_graceful_shutdown(async{let _=tokio::signal::ctrl_c().await;})=>result?,_=snap_document_local::web::dispatch(documents)=>unreachable!()}
    Ok(())
}
