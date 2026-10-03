//! Platform composition and physical providers for portable Factorio declarations.
use snap_document::runtime::Runtime;
use snap_identity::oauth as rp;
use snap_store::{Error, Transaction};

pub fn session_id(tx: &mut Transaction<'_>, bearer: &str) -> Result<(String, bool), Error> {
    factorio::login::session_id(tx, bearer, crate::now())
}
pub fn session(tx: &mut Transaction<'_>, bearer: &str) -> Result<(rp::Grant, bool), Error> {
    factorio::login::session(tx, bearer, crate::now())
}
pub fn retained(tx: &mut Transaction<'_>, bearer: &str) -> Result<String, Error> {
    factorio::login::retained(tx, bearer, crate::now())
}
pub fn register(
    mut host: Runtime<snap_store_sqlite::Sqlite>,
    config: factorio::Config,
    origin: String,
) -> Runtime<snap_store_sqlite::Sqlite> {
    host = host.with_inputs(|key| match key {
        "clock" => Ok(serde_json::json!(crate::now())),
        "entropy" | "proof" => Ok(serde_json::json!(crate::random())),
        _ => Err(snap_transport::Error::Unavailable),
    });
    let app = factorio::application(config, origin);
    for definition in app.preconnection {
        host = host.with_preconnection_request(definition);
    }
    for definition in app.requests {
        host = host.with_request(definition);
    }
    host
}
