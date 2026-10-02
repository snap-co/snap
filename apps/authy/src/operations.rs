//! Account operations over Authy's metadata interface. Authentication facts come
//! from Transport's provider; this app does not resolve credentials or sessions.
use alloc::{vec, vec::Vec};
use snap_transport::{Operation, operation::Definition};

pub struct FetchAccount;
impl Operation for FetchAccount {
    const NAME: &'static str = "authy.account";
    type Input = ();
    type Output = crate::Account;
    type Error = ();
    type Progress = ();
}
pub fn declarations() -> Vec<Definition> {
    vec![Definition::typed::<FetchAccount>(
        true,
        vec![],
        crate::Account::data(),
        &[],
        |tx, _, context| {
            let principal = context
                .principal
                .as_ref()
                .ok_or(snap_store::Error::NotFound)?;
            Ok(crate::account_by_identity(
                tx,
                &principal.identity,
                principal.authenticated_at,
            )?)
        },
    )]
}

pub fn http_routes() -> Vec<snap_transport::carrier::HttpRoute> {
    vec![snap_transport::carrier::HttpRoute {
        operation: FetchAccount::NAME,
        method: snap_transport::carrier::HttpMethod::Get,
        read_bearer: true,
    }]
}
