//! Rust SDK projection of a relying-party's public authentication view.
//! OAuth grants, refresh authority and stored session records remain host-private.
use serde_json::Value;
use snap_store::Error;
pub fn project(value: Value) -> Result<Option<Value>, Error> {
    match value.get("identified").and_then(Value::as_bool) {
        Some(true) => Ok(Some(value)),
        Some(false) => Ok(None),
        None => Err(Error::Invalid),
    }
}
