//! A portable multi-module signup. Every helper shares the caller's transaction.
//! No host callbacks or database handles enter application code.
use alloc::vec;
use snap_store::resident::{Error, Transaction};
use snap_store::{Row, Value};

pub const TABLES: [&str; 5] = [
    "signup.accounts",
    "signup.grants",
    "signup.documents",
    "signup.outbox",
    "signup.policy",
];

pub fn create(tx: &mut Transaction<'_>, id: i64, email: &str) -> Result<(), Error> {
    if tx.get(TABLES[0], &[id.into()])?.is_some() {
        return Err(Error::Constraint);
    }
    account(tx, id, email)?;
    grant(tx, id)?;
    document(tx, id)?;
    // Deliberately read after staging several modules' writes. A cold policy
    // causes ALL those writes to be discarded, including the new account.
    let policy = tx.get(TABLES[4], &[0.into()])?.ok_or(Error::NotFound)?;
    if policy["enabled"] != Value::Integer(1) {
        return Err(Error::Unavailable);
    }
    // Durable intent to notify, not the email itself. A future delivery worker
    // consumes this outbox after commit and needs its own delivery/retry policy.
    tx.insert(
        TABLES[3],
        Row::from_iter(vec![
            ("id".into(), id.into()),
            ("kind".into(), "welcome".into()),
        ]),
    )
}
fn account(tx: &mut Transaction<'_>, id: i64, email: &str) -> Result<(), Error> {
    tx.insert(
        TABLES[0],
        Row::from_iter(vec![
            ("id".into(), id.into()),
            ("email".into(), email.into()),
        ]),
    )
}
fn grant(tx: &mut Transaction<'_>, id: i64) -> Result<(), Error> {
    tx.insert(
        TABLES[1],
        Row::from_iter(vec![
            ("account".into(), id.into()),
            ("role".into(), "owner".into()),
        ]),
    )
}
fn document(tx: &mut Transaction<'_>, id: i64) -> Result<(), Error> {
    tx.insert(
        TABLES[2],
        Row::from_iter(vec![
            ("owner".into(), id.into()),
            ("title".into(), "Home".into()),
        ]),
    )
}
