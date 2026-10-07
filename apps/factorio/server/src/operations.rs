//! Platform composition and physical providers for portable Factorio declarations.
use snap_identity::oauth as rp;
use snap_store::{Error, Transaction};
use snap_transport::operation::Registry;

pub fn session_id(tx: &mut Transaction<'_>, bearer: &str) -> Result<(String, bool), Error> {
    factorio::login::session_id(tx, bearer, crate::now())
}
pub fn session(tx: &mut Transaction<'_>, bearer: &str) -> Result<(rp::Grant, bool), Error> {
    factorio::login::session(tx, bearer, crate::now())
}
pub fn retained(tx: &mut Transaction<'_>, bearer: &str) -> Result<String, Error> {
    factorio::login::retained(tx, bearer, crate::now())
}
pub struct Authority;
impl snap_transport::bearer::Authority for Authority {
    fn identify(&self, tx: &mut Transaction<'_>, bearer: &str) -> Result<String, Error> {
        session(tx, bearer).map(|(s, _)| s.owner)
    }
    fn retained(&self, tx: &mut Transaction<'_>, bearer: &str) -> Result<String, Error> {
        retained(tx, bearer)
    }
    fn resolve(
        &self,
        tx: &mut Transaction<'_>,
        bearer: Option<&str>,
        required: bool,
    ) -> Result<snap_transport::bearer::Resolved, Error> {
        let actor = match bearer {
            Some(bearer) => match self.identify(tx, bearer) {
                Ok(actor) => Some(actor),
                Err(Error::NotFound) if !required => None,
                Err(error) => return Err(error),
            },
            None => None,
        };
        Ok(snap_transport::bearer::Resolved {
            actor,
            principal: None,
        })
    }
}
pub fn register(mut operations: Registry, config: factorio::Config, origin: String) -> Registry {
    let app = factorio::application(config, origin);
    for definition in app.preconnection {
        operations = operations.with_preconnection_request(definition);
    }
    for definition in app.requests {
        operations = operations.with_request(definition);
    }
    operations
}

pub fn inputs(key: &str) -> Result<snap_transport::Value, snap_transport::Error> {
    match key {
        "clock" => Ok(serde_json::json!(crate::now())),
        "entropy" | "proof" => Ok(serde_json::json!(crate::random())),
        _ => Err(snap_transport::Error::Unavailable),
    }
}
