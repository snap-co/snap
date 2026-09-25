//! Authy's portable password/session application definition.
#![no_std]
extern crate alloc;
pub mod account;
pub mod http;

pub fn server<S: snap_store::Store, C: snap_runtime::passport::Crypto, K: snap_store::Cache>(
    store: S,
    crypto: C,
    cache: K,
) -> snap_runtime::passport::Passport<S, C, K> {
    snap_runtime::passport::Passport::new("user", "account.create", store, crypto, cache)
        .with_enrollment(account::enrollment)
}

pub fn schemas() -> alloc::vec::Vec<snap_store::Schema> {
    let mut schemas = snap_runtime::passport::schemas().to_vec();
    schemas.push(account::schema());
    schemas.extend(snap_oidc::storage::schemas());
    schemas
}

pub fn client() -> snap_client::identity::Client {
    snap_client::identity::Client::new("account.create")
}
