//! Identity vocabulary. Independent of Passport, Transport, storage and carriers.
#![no_std]
extern crate alloc;
use alloc::string::String;
use serde::{Deserialize, Serialize};

pub const OPERATIONS: [snap_protocol::Operation; 5] = [
    snap_protocol::Operation {
        key: "identity.fetch",
    },
    snap_protocol::Operation {
        key: "identity.password.acquire",
    },
    snap_protocol::Operation {
        key: "identity.release",
    },
    snap_protocol::Operation {
        key: "identity.credentials",
    },
    snap_protocol::Operation {
        key: "identity.sessions",
    },
];

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
