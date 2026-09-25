//! Authy's portable password/session application definition.
#![no_std]

pub fn server<S: snap_store::Store, C: snap_runtime::passport::Crypto, K: snap_store::Cache>(
    store: S,
    crypto: C,
    cache: K,
) -> snap_runtime::passport::Passport<S, C, K> {
    snap_runtime::passport::Passport::new("user", "account.create", store, crypto, cache)
}

pub fn client() -> snap_client::identity::Client {
    snap_client::identity::Client::new("account.create")
}
