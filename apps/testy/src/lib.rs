//! Snap's portable contract application. Store is an opt-in consumer; hosts own IO.
#![no_std]
extern crate alloc;

mod client;
mod program;
#[cfg(feature = "store")]
pub mod store;

pub use client::{Client, journey};
pub use program::{App, CEILING};

pub struct Health;
impl snap_transport::Operation for Health {
    const NAME: &'static str = "health.up";
    const HTTP: Option<(snap_transport::carrier::HttpMethod, bool)> =
        Some((snap_transport::carrier::HttpMethod::Get, false));
    type Input = ();
    type Output = snap_transport::Value;
    type Error = ();
    type Progress = ();
}

use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_transport::{Value, server::Authority};

pub const BEARER: &str = "testy-private-fixture-token";
pub const IDENTITY: &str = "testy-fixture-identity";
pub struct TestAuthority;
impl Authority for TestAuthority {
    fn identify(&self, bearer: &str) -> Result<String, snap_transport::Error> {
        (bearer == BEARER)
            .then(|| IDENTITY.into())
            .ok_or(snap_transport::Error::InvalidBearer)
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
    value.is_null()
}
