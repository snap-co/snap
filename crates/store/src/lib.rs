//! Server-side resident storage and durable cross-module transactions.
#![no_std]
extern crate alloc;

mod data;
pub mod migration;
pub use data::Data;
mod schema;
mod transaction;

use alloc::{collections::BTreeMap, string::String, vec::Vec};
pub use schema::*;
pub use transaction::*;

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
