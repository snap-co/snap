//! Chatty's existing upstream-access policy over Identity's credential provider.
//! Optional acquisition may treat NotFound as anonymous; connected/protected
//! work still requires the same live local session and upstream access lease.
use snap_identity::{Identity, Principal, oauth};
use snap_store::{Error, Transaction};

pub struct Credentials;
impl snap_transport::bearer::Provider for Credentials {
    fn data(&self) -> snap_store::Data {
        oauth::data()
    }
    fn identify(
        &self,
        tx: &mut Transaction<'_>,
        bearer: &str,
        now: i64,
    ) -> Result<Principal, Error> {
        let principal = Identity::default().resolve(tx, &snap_crypto::Native, bearer, now)?;
        // Assertion sessions have no upstream refresh grant and expire in five
        // minutes. OAuth sessions retain the existing upstream access policy.
        if tx
            .get("identity.oauth_grants", &[oauth::digest(bearer).into()])?
            .is_some()
        {
            oauth::lease(tx, &oauth::digest(bearer), now)?;
        }
        Ok(principal)
    }
}
