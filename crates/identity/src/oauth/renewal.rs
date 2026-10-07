//! Trusted credential preparation requests refresh through committed private rows.
//! The controller commits its claim before rotation IO. Interrupted claims are
//! revoked by startup recovery, never replayed or treated as session authority.
use super::Grant;
use crate::attempt;
use alloc::{format, string::String};
use serde::{Deserialize, Serialize};
use snap_store::{Error, Row, Transaction};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    previous: Grant,
    claimed: bool,
}
fn key(id: &str) -> String {
    format!("oauth-renewal:{id}")
}

/// Host-only preparation. Validate the application credential before selecting
/// this private session ID. Reconciliation finishes before the host call returns.
pub fn request(tx: &mut Transaction<'_>, id: &str, now: i64) -> Result<(), Error> {
    if let Some(previous) = super::begin_refresh_id(tx, id, now)? {
        attempt::write(
            tx,
            super::ATTEMPTS,
            &key(id),
            &Record {
                previous,
                claimed: false,
            },
            true,
        )?;
    }
    Ok(())
}
pub(super) fn recover(tx: &mut Transaction<'_>) -> Result<(), Error> {
    for row in tx.find(super::ATTEMPTS, "primary", &[])? {
        if let Some(snap_store::Value::Text(id)) = row.get("id")
            && id.starts_with("oauth-renewal:")
        {
            tx.delete(super::ATTEMPTS, &[id.clone().into()])?;
        }
    }
    Ok(())
}
pub(super) fn pending(row: &Row) -> bool {
    row.get("data")
        .and_then(|v| match v {
            snap_store::Value::Text(s) => serde_json::from_str::<Record>(s).ok(),
            _ => None,
        })
        .is_some_and(|r| !r.claimed)
}
pub(super) fn claim(tx: &mut Transaction<'_>, key: &str, now: i64) -> Result<Grant, Error> {
    let mut record: Record = attempt::read(tx, super::ATTEMPTS, key)?.ok_or(Error::NotFound)?;
    let current = super::retained(tx, &record.previous.id, now)?;
    if record.claimed || !current.refreshing || current.version != record.previous.version {
        return Err(Error::NotFound);
    }
    record.claimed = true;
    attempt::write(tx, super::ATTEMPTS, key, &record, false)?;
    Ok(current)
}
pub(super) fn clear(tx: &mut Transaction<'_>, key: &str) -> Result<(), Error> {
    tx.delete(super::ATTEMPTS, &[key.into()])?;
    Ok(())
}
