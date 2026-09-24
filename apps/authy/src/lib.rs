//! Authy's portable password/session application definition.
#![no_std]

pub fn server<S: snap_store::Store, C: snap_runtime::passport::Crypto>(
    store: S,
    crypto: C,
) -> snap_runtime::passport::Passport<S, C> {
    // Authoritative reads are the initial policy; cached snapshots remain optional.
    snap_runtime::passport::Passport::new(
        "user",
        "account.create",
        store,
        crypto,
        snap_store::NoCache,
    )
}

pub fn client() -> snap_client::identity::Client {
    snap_client::identity::Client::new("account.create")
}
