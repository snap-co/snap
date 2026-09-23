//! The small part of Snap's protocol needed by Healthy. No platform dependencies.
#![no_std]

extern crate alloc;

use alloc::string::String;
use serde::{Deserialize, Serialize};

pub use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Query,
}

#[derive(Clone, Copy, Debug)]
pub struct Operation {
    pub key: &'static str,
    pub lane: Lane,
}

/// Carrier framing has already been removed. Missing payload differs from JSON null.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invocation {
    pub operation_id: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
}

/// Only errors exercised by this slice. Extend alongside the behavior that needs them.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum Error {
    InvalidInputError { message: String },
    ContractViolationError { message: String },
    UnavailableError { message: String },
}

pub type Outcome = Result<Value, Error>;

/// The existing TypeScript SDK expects a completion Event, even over HTTP.
#[derive(Debug, Serialize)]
pub struct Completion {
    key: &'static str,
    target: String,
    payload: Value,
}

impl Completion {
    pub fn new(operation_id: String, outcome: Outcome) -> Self {
        let payload = match outcome {
            Ok(payload) => json!({ "ok": true, "payload": payload }),
            Err(error) => json!({ "ok": false, "error": error }),
        };
        Self {
            key: "transport.complete",
            target: operation_id,
            payload,
        }
    }
}
