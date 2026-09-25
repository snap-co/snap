//! Local storage contract. Declarations do not publish remote operations.
#![no_std]
extern crate alloc;
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::future::Future;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Table {
    pub namespace: &'static str,
    pub name: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Integer,
    Bytes,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
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

/// Identifiers use nonempty lowercase ASCII letters, digits and underscores.
/// Backends must reject ambiguous physical mappings during registration.
#[derive(Clone, Debug)]
pub struct Schema {
    pub table: Table,
    pub columns: &'static [(&'static str, Kind)],
    pub primary: &'static [&'static str],
    pub indexes: &'static [Index],
    pub foreign: &'static [ForeignKey],
    /// A one-time physical-name migration. Only the backend interprets this.
    /// Registration must atomically preserve all rows or fail without changes.
    pub legacy_name: Option<&'static str>,
}
#[derive(Clone, Debug)]
pub struct Index {
    pub columns: &'static [&'static str],
    pub unique: bool,
}
#[derive(Clone, Debug)]
pub struct ForeignKey {
    pub columns: &'static [&'static str],
    pub target: Table,
    pub references: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Compare {
    Eq,
    Ne,
    Gt,
    Le,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Predicate {
    pub column: &'static str,
    pub compare: Compare,
    pub value: Value,
}
impl Predicate {
    pub fn eq(column: &'static str, value: impl Into<Value>) -> Self {
        Self {
            column,
            compare: Compare::Eq,
            value: value.into(),
        }
    }
    pub fn gt(column: &'static str, value: i64) -> Self {
        Self {
            column,
            compare: Compare::Gt,
            value: value.into(),
        }
    }
    pub fn le(column: &'static str, value: i64) -> Self {
        Self {
            column,
            compare: Compare::Le,
            value: value.into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Query {
    pub table: Table,
    pub filter: Vec<Predicate>,
    pub order: Vec<&'static str>,
    pub limit: u32,
}
impl Query {
    pub fn new(table: Table) -> Self {
        Self {
            table,
            filter: Vec::new(),
            order: Vec::new(),
            limit: 1024,
        }
    }
    pub fn matching(mut self, filter: Vec<Predicate>) -> Self {
        self.filter = filter;
        self
    }
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = limit;
        self
    }
    pub fn ordered(mut self, columns: &[&'static str]) -> Self {
        self.order = columns.into();
        self
    }
}

#[derive(Clone, Debug)]
pub enum Statement {
    Select(Query),
    Insert {
        table: Table,
        row: Row,
    },
    Update {
        table: Table,
        filter: Vec<Predicate>,
        changes: Row,
    },
    Delete {
        table: Table,
        filter: Vec<Predicate>,
    },
}
#[derive(Clone, Debug)]
pub struct Guard {
    pub query: Query,
    pub exists: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Transaction {
    pub guards: Vec<Guard>,
    pub statements: Vec<Statement>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Conflict,
    Constraint,
    Invalid,
    Unavailable,
}

/// All guards and statements execute in one serializable transaction. Guards run
/// before any statement; failure rolls back every write. Selects share its snapshot
/// and observe earlier statements. Results have one entry per statement, empty for
/// writes. Ordering is explicit; without it only primary-key order is promised.
/// Implementations reject undeclared tables/columns and mismatched value types.
/// Memory is an explicitly non-durable implementation of the same atomic semantics.
/// A successful durable backend result means commit completed, not merely queued.
pub trait Store: Clone + Send + Sync + 'static {
    fn transaction(
        &self,
        transaction: Transaction,
    ) -> impl Future<Output = Result<Vec<Rows>, Error>> + Send;
}

/// Advisory snapshots only. Transactional reads always go to Store authority.
/// Cache loss is harmless; freshness must be revalidated before granting authority.
pub trait Cache: Clone + Send + Sync + 'static {
    fn get(&self, query: &Query) -> Option<Rows>;
    fn put(&self, query: Query, rows: Rows);
}
#[derive(Clone)]
pub struct NoCache;
impl Cache for NoCache {
    fn get(&self, _: &Query) -> Option<Rows> {
        None
    }
    fn put(&self, _: Query, _: Rows) {}
}
pub async fn snapshot(store: &impl Store, cache: &impl Cache, query: Query) -> Result<Rows, Error> {
    if let Some(rows) = cache.get(&query) {
        return Ok(rows);
    }
    let mut result = store
        .transaction(Transaction {
            guards: Vec::new(),
            statements: alloc::vec![Statement::Select(query.clone())],
        })
        .await?;
    let rows = result.pop().ok_or(Error::Unavailable)?;
    cache.put(query, rows.clone());
    Ok(rows)
}
