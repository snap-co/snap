//! Authy's portable password/session application definition.
#![no_std]

pub fn server() -> snap_runtime::passport::Passport {
    snap_runtime::passport::Passport::new("user", "account.create")
}

pub fn client() -> snap_client::identity::Client {
    snap_client::identity::Client::new("account.create")
}
