use axum::{Router, extract::State, http::HeaderMap, response::Response, routing::get};
use serde_json::json;
type Host<B> = snap_host::Blocking<B, snap_document::host::Documents<B>>;
use snap_identity::oauth as rp;
use snap_identity_native::oauth::{Cookies, OAuth, failure, no_store, now, random};
use snap_store::Error;
use snap_transport_native::{Dispatcher, Shared};
use snap_transport_ws::Service;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};

async fn session(
    State(oauth): State<Arc<OAuth<Host<snap_store_sqlite::Sqlite>>>>,
    headers: HeaderMap,
) -> Response {
    match oauth.session(&headers).await {
        Ok(session) => no_store(
            json!({"identified":true,"csrf":session.csrf,"account":{"id":session.subject,"owner":session.owner,"name":session.profile["name"],"email":session.profile["email"]}}),
        ),
        Err(Error::NotFound) => no_store(json!({"identified":false})),
        Err(error) => failure(error),
    }
}

fn operations(
    document: Arc<snap_document::server::Document>,
) -> snap_transport::operation::Registry {
    let mut operations = snap_transport::operation::Registry::default();
    for definition in snap_document::operations::definitions(document)
        .into_iter()
        .chain(chatty::operations::declarations())
    {
        operations = operations.with_request(definition);
    }
    operations
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    oauth: snap_identity_native::oauth::Settings,
}
fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut values: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        snap_identity::MIGRATION,
        snap_identity_native::oauth::MIGRATION,
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
        snap_store_sqlite::migrate(&database, &migrations())?;
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
    let origin = snap_identity_native::oauth::origin(&config.host.public_origin(address))?;
    let mut store = snap_store_sqlite::Sqlite::open(&database)?;
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
        .chain(chatty::TABLES.iter())
    {
        store.load(table)?;
    }
    let cookies = Cookies::load(&mut store, "chatty", origin.starts_with("https:"))?;
    let document = Arc::new(chatty::document());
    let operations = operations(document.clone());
    let host = Host::new(
        store,
        snap_document::host::Documents::new(document),
        operations,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
            |tx, bearer| rp::lease(tx, &rp::digest(bearer), now()).map(|s| s.owner),
        ))),
        snap_transport::server::Config::default(),
        random(),
    )
    .with_inputs(|key| match key {
        "clock" => Ok(json!(now())),
        _ => Err(snap_transport::Error::Unavailable),
    });
    let documents = Shared::new(host);
    let transport = Arc::new(Service {
        dispatch: Dispatcher::web(documents.clone()),
        origin: origin.clone(),
        cookie: Some(cookies.reader()),
        require_cookie: false,
    });
    let oauth = OAuth::new(
        documents.clone(),
        cookies,
        snap_identity_native::oauth::Config {
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
        .merge(snap_transport_ws::router(transport))
        .fallback_service(
            ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
        );
    println!("Chatty http://{address}");
    tokio::select! {result=axum::serve(listener,router).with_graceful_shutdown(async{let _=tokio::signal::ctrl_c().await;})=>result?,_=snap_transport_native::dispatch(documents)=>unreachable!()}
    Ok(())
}
