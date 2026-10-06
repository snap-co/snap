use axum::{Router, extract::State, http::HeaderMap, response::Response, routing::get};
use serde_json::json;
type Host<B> = snap_transport::host::Blocking<
    B,
    snap_transport::host::Controllers<B, snap_transport::replication::Replications>,
>;
use snap_identity::oauth as rp;
use snap_identity_native::oauth::{Cookies, OAuth, failure, no_store, now, random};
use snap_store::Error;
use snap_transport::native::{PendingListener, Server, WebSocket};
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
    replication: &Arc<snap_transport::replication::Registry>,
) -> snap_transport::operation::Registry {
    let mut operations =
        snap_transport::operation::Registry::default().with_request(replication.operation());
    for definition in chatty::operations::declarations() {
        operations = operations.with_request(definition);
    }
    operations
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    oauth: snap_identity_native::oauth::Settings,
    tcp: Option<snap_config::Tcp>,
}
fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut values: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_store::resource::MIGRATION,
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
    let listener = PendingListener::reserve(config.host.listen)?;
    let address = listener.local_addr()?;
    let origin = snap_identity_native::oauth::origin(&config.host.public_origin(address))?;
    let mut store = snap_store_sqlite::Sqlite::open(&database)?;
    for table in snap_access::TABLES
        .iter()
        .chain(core::iter::once(&snap_store::resource::TABLE))
        .chain(rp::TABLES.iter())
        .chain(chatty::TABLES.iter())
    {
        store.load(table)?;
    }
    let cookies = Cookies::load(&mut store, "chatty", origin.starts_with("https:"))?;
    let replication = chatty::replication();
    let jwks = snap_identity_native::assertion::authy_keys(&config.app.oauth.issuer).await?;
    let operations =
        operations(&replication).with_preconnection_request(snap_identity::assertion::operation(
            config.app.oauth.issuer.clone(),
            config.app.oauth.client_id.clone(),
            jwks,
            || snap_crypto::Native,
        ));
    let tcp = config
        .app
        .tcp
        .as_ref()
        .map(|tcp| -> Result<_, Box<dyn std::error::Error>> {
            Ok((
                PendingListener::reserve(tcp.listen)?,
                snap_transport::native::tls::ServerTls::new(
                    &config.path(&tcp.cert_file),
                    &config.path(&tcp.key_file),
                )?,
            ))
        })
        .transpose()?;
    let host = Host::new(
        store,
        snap_transport::host::Controllers::around(snap_transport::replication::Replications::new(
            replication,
        )),
        operations,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
            |tx, bearer| {
                let principal = snap_identity::Identity::default().resolve(
                    tx,
                    &snap_crypto::Native,
                    bearer,
                    now(),
                )?;
                // OAuth sessions must retain upstream access policy. Assertion
                // sessions have no upstream refresh grant and expire in five minutes.
                if tx
                    .get("identity.oauth_grants", &[rp::digest(bearer).into()])?
                    .is_some()
                {
                    rp::lease(tx, &rp::digest(bearer), now())?;
                }
                Ok(principal.identity)
            },
        ))),
        snap_transport::server::Config::default(),
        random(),
    )
    .with_inputs(|key| match key {
        "clock" => Ok(json!(now())),
        _ => Err(snap_transport::Error::Unavailable),
    });
    let server = Server::new(host).await?;
    let transport = WebSocket {
        origin: origin.clone(),
        cookie: Some(cookies.reader()),
        require_cookie: false,
    };
    let oauth = OAuth::new(
        server.transactions(),
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
        .merge(server.websocket(transport))
        .fallback_service(
            ServeDir::new(&assets).fallback(ServeFile::new(format!("{assets}/index.html"))),
        );
    let listener = listener.listen()?;
    println!("Chatty http://{address}");
    let http = axum::serve(listener, router).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    if let Some((listener, tls)) = tcp {
        println!("Chatty tls://{}", listener.local_addr()?);
        server.run(listener.listen()?, tls, None, http).await?;
    } else {
        server.run_http(http).await?;
    }
    Ok(())
}
