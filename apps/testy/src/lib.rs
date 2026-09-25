//! Snap's transport/execution contract application. No Identity, Store or host dependencies.
#![no_std]
extern crate alloc;

mod client;
mod program;

pub use client::{Client, journey};
pub use program::{App, CEILING};

use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_transport::{Value, server::Authority};

pub const BEARER: &str = "testy-private-fixture-token";
pub const IDENTITY: &str = "testy-fixture-identity";
pub struct TestAuthority;
impl Authority for TestAuthority {
    fn identify(&self, bearer: &str) -> Option<String> {
        (bearer == BEARER).then(|| IDENTITY.into())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub operation: String,
    pub operand: i64,
    pub before: i64,
    pub after: i64,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Calculator {
    pub accumulator: i64,
    pub history: Vec<Entry>,
}
fn start_output(value: &Value) -> bool {
    value.as_object().is_some_and(|obj| {
        obj.len() == 1 && obj.get("bearer").and_then(Value::as_str) == Some(BEARER)
    })
}
