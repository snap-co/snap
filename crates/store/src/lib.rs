//! Server-side resident storage and durable cross-module transactions.
#![no_std]
extern crate alloc;

mod data;
pub mod inbox;
pub mod migration;
pub mod residency;
pub mod resource;
pub use data::Data;
mod schema;
mod transaction;

use alloc::{collections::BTreeMap, string::String, vec::Vec};
pub use schema::*;
pub use transaction::*;

/// Internal transactions supplied by application execution. Implementations
/// serialize these behind accepted work and return values only after durable
/// commit and required application reconciliation. Callbacks must not perform
/// external IO or reenter their execution host.
pub trait Host {
    fn transact<T>(
        &mut self,
        operation: &str,
        handler: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    Integer,
    Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Value {
    Text(String),
    Integer(i64),
    Bytes(Vec<u8>),
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}
impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}
impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

pub type Row = BTreeMap<String, Value>;
pub type Rows = Vec<Row>;
