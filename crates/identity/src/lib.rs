//! Identity vocabulary. Independent of Passport, Transport, storage and carriers.
#![no_std]
extern crate alloc;
use alloc::string::String;
use serde::{Deserialize, Serialize};
use snap_protocol::{Error, IdentityPolicy, Operation, Value};

pub const OPERATIONS: [snap_protocol::Operation; 5] = [
    Operation::new("identity.fetch", void, IdentityPolicy::Optional),
    Operation::new(
        "identity.password.acquire",
        password,
        IdentityPolicy::Anonymous,
    ),
    Operation::new("identity.release", release, IdentityPolicy::Required),
    Operation::new("identity.credentials", void, IdentityPolicy::Required),
    Operation::new("identity.sessions", void, IdentityPolicy::Required),
];

fn invalid() -> Error {
    Error::InvalidInputError {
        message: "Invalid input".into(),
    }
}
pub fn void(value: &Option<Value>) -> Result<(), Error> {
    if value.as_ref().is_none_or(Value::is_null) {
        Ok(())
    } else {
        Err(invalid())
    }
}
pub fn password(value: &Option<Value>) -> Result<(), Error> {
    let value = value.as_ref().ok_or_else(invalid)?;
    if value.get("email").and_then(Value::as_str).is_none()
        || value.get("password").and_then(Value::as_str).is_none()
    {
        return Err(invalid());
    }
    Ok(())
}
pub fn enrollment(value: &Option<Value>) -> Result<(), Error> {
    password(value)?;
    let password = value
        .as_ref()
        .and_then(|v| v["password"].as_str())
        .ok_or_else(invalid)?;
    if (8..=256).contains(&password.encode_utf16().count()) {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn release(value: &Option<Value>) -> Result<(), Error> {
    serde_json::from_value::<Release>(value.clone().ok_or_else(invalid)?)
        .map(|_| ())
        .map_err(|_| invalid())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_id: String,
    pub identity_id: String,
    pub expires_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "camelCase")]
pub enum Release {
    Current,
    Others,
    All,
    Session {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}
