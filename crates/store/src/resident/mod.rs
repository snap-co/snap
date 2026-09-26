//! Server-side resident transactions. Handlers receive only `Transaction`, never
//! a backend. A miss poisons the attempt, even if its Result is caught. There is
//! no suspension or implicit retry. Hosts explicitly load, then callers may retry.
//! This is cooperative IO isolation, not a sandbox for arbitrary Rust callbacks.
pub mod migration;
mod schema;
use crate::{Row, Rows, Value};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};
pub use schema::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lookup {
    pub table: String,
    pub index: String,
    /// An ordered index prefix. Empty means the entire index.
    pub prefix: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Miss(Lookup),
    Invalid,
    Constraint,
    NotFound,
    Unavailable,
    /// Commit outcome is unknown. This Store is fenced until reopened/recovered.
    Indeterminate,
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Store {self:?}")
    }
}
impl core::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    Insert {
        table: String,
        row: Row,
    },
    Update {
        table: String,
        key: Vec<Value>,
        row: Row,
    },
    Delete {
        table: String,
        key: Vec<Value>,
    },
}

pub enum CommitError {
    /// Backend guarantees no write from this transaction committed.
    Rejected(Error),
    Indeterminate,
}

/// Host IO seam. The backend owns exclusive write authority for its lifetime.
/// `load` returns the COMPLETE table from that authority, never a partial page.
/// `commit` atomically persists all writes, in order, before returning success.
/// Failure must distinguish confirmed rollback from an unknown commit outcome.
/// No other connection, process, or out-of-band writer may modify the database
/// while resident data is served. The SQLite adapter enforces an exclusive lock.
pub trait Backend {
    fn load(&mut self, table: &Table) -> Result<Rows, Error>;
    fn commit(&mut self, writes: &[Write]) -> Result<(), CommitError>;
}

#[derive(Clone, Default)]
struct Resident {
    rows: BTreeMap<Vec<Value>, Row>,
    absent: BTreeSet<Vec<Value>>,
    complete: bool,
    indexes: BTreeMap<String, BTreeMap<Vec<Value>, BTreeSet<Vec<Value>>>>,
}

impl Resident {
    fn reindex(&mut self, table: &Table) {
        self.indexes.clear();
        for (name, columns) in core::iter::once(("primary", &table.primary))
            .chain(table.indexes.iter().map(|i| (i.name.as_str(), &i.columns)))
        {
            let mut index: BTreeMap<Vec<Value>, BTreeSet<Vec<Value>>> = BTreeMap::new();
            for (primary, row) in &self.rows {
                index
                    .entry(columns.iter().map(|c| row[c].clone()).collect())
                    .or_default()
                    .insert(primary.clone());
            }
            self.indexes.insert(name.into(), index);
        }
    }
}

/// Bounded diagnostic history with a lifetime count. Values may contain sensitive
/// lookup data; hosts decide whether/how to export them. No implicit logging IO.
#[derive(Default)]
pub struct Misses {
    pub count: u64,
    pub recent: Vec<MissRecord>,
}
pub struct MissRecord {
    pub operation: String,
    pub lookup: Lookup,
}

/// Returned only after durable commit AND resident publication. External effects
/// can begin after this result. For crash-safe delivery, write an outbox record in
/// the same transaction; an in-process success callback alone is not durable.
#[derive(Debug)]
pub struct Committed<T> {
    pub value: T,
}

pub struct Store<B> {
    catalog: Catalog,
    backend: B,
    residents: BTreeMap<String, Resident>,
    misses: Misses,
    fenced: bool,
}

impl<B: Backend> Store<B> {
    pub fn new(catalog: Catalog, backend: B) -> Result<Self, Error> {
        catalog.validate()?;
        Ok(Self {
            residents: catalog
                .tables
                .iter()
                .map(|t| (t.name.clone(), Resident::default()))
                .collect(),
            catalog,
            backend,
            misses: Misses::default(),
            fenced: false,
        })
    }
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    pub fn misses(&self) -> &Misses {
        &self.misses
    }
    pub fn take_misses(&mut self) -> Vec<MissRecord> {
        core::mem::take(&mut self.misses.recent)
    }

    /// Explicit host action, separate from an operation. Resolves absence as well
    /// as presence. Repeated loads cannot race a commit because both borrow Store
    /// exclusively. No eviction or expiry: residency is durable-state knowledge.
    pub fn load(&mut self, table: &str) -> Result<(), Error> {
        if self.fenced {
            return Err(Error::Indeterminate);
        }
        let schema = self.catalog.table(table)?;
        let rows = self.backend.load(schema)?;
        let mut resident = Resident {
            complete: true,
            ..Resident::default()
        };
        for row in rows {
            schema.validate_row(&row)?;
            if resident.rows.insert(schema.key(&row), row).is_some() {
                return Err(Error::Invalid);
            }
        }
        resident.reindex(schema);
        self.residents.insert(table.into(), resident);
        Ok(())
    }

    /// Exclusive operation gate. Scratch state cannot escape as a borrowed live
    /// record. Errors and unwinding discard it; no host write begins until the
    /// closure returns Ok and the transaction has no sticky failure.
    pub fn run<T>(
        &mut self,
        operation: &str,
        handler: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<Committed<T>, Error> {
        if self.fenced {
            return Err(Error::Indeterminate);
        }
        let mut tx = Transaction {
            catalog: &self.catalog,
            residents: self.residents.clone(),
            writes: Vec::new(),
            failed: None,
        };
        let result = handler(&mut tx);
        let result = if let Some(error) = tx.failed {
            Err(error)
        } else {
            result
        };
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                if let Error::Miss(lookup) = &error {
                    self.misses.count = self.misses.count.saturating_add(1);
                    if self.misses.recent.len() == 128 {
                        self.misses.recent.remove(0);
                    }
                    self.misses.recent.push(MissRecord {
                        operation: operation.into(),
                        lookup: lookup.clone(),
                    });
                }
                return Err(error);
            }
        };
        if !tx.writes.is_empty() {
            // Also fence unwinding out of a host commit: its effects may already
            // be durable even though the host never returned a classified result.
            self.fenced = true;
            match self.backend.commit(&tx.writes) {
                Ok(()) => self.fenced = false,
                Err(CommitError::Rejected(error)) => {
                    self.fenced = false;
                    return Err(error);
                }
                Err(CommitError::Indeterminate) => {
                    self.fenced = true;
                    self.residents.clear();
                    return Err(Error::Indeterminate);
                }
            }
        }
        // All allocations/index construction happened BEFORE durable commit.
        // Exclusive borrowing prevents a reader between commit and publication.
        self.residents = tx.residents;
        Ok(Committed { value })
    }
}

pub struct Transaction<'a> {
    catalog: &'a Catalog,
    residents: BTreeMap<String, Resident>,
    writes: Vec<Write>,
    failed: Option<Error>,
}

impl Transaction<'_> {
    fn check<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, Error>) -> Result<T, Error> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        let result = f(self);
        if let Err(error) = &result {
            self.failed = Some(error.clone());
        }
        result
    }

    pub fn get(&mut self, table: &str, key: &[Value]) -> Result<Option<Row>, Error> {
        self.check(|this| {
            let schema = this.catalog.table(table)?;
            if key.len() != schema.primary.len() {
                return Err(Error::Invalid);
            }
            let mut rows = this.lookup(table, "primary", key)?;
            Ok(rows.pop())
        })
    }

    /// Complete matching set, ordered by index key then primary key. Prefixes
    /// include composite keys; an empty prefix scans a declared index. A partial
    /// resident set NEVER masquerades as a complete secondary-index result.
    pub fn find(&mut self, table: &str, index: &str, prefix: &[Value]) -> Result<Rows, Error> {
        self.check(|this| this.lookup(table, index, prefix))
    }

    fn lookup(&self, table: &str, index: &str, prefix: &[Value]) -> Result<Rows, Error> {
        let schema = self.catalog.table(table)?;
        let columns = schema.index(index)?;
        if prefix.len() > columns.len()
            || columns
                .iter()
                .zip(prefix)
                .any(|(c, v)| schema.column(c).map(|c| c.kind) != Ok(kind(v)))
        {
            return Err(Error::Invalid);
        }
        let resident = &self.residents[table];
        if index == "primary" && prefix.len() == columns.len() {
            if let Some(row) = resident.rows.get(prefix) {
                return Ok(alloc::vec![row.clone()]);
            }
            if resident.complete || resident.absent.contains(prefix) {
                return Ok(Vec::new());
            }
        }
        if !resident.complete {
            return Err(Error::Miss(Lookup {
                table: table.into(),
                index: index.into(),
                prefix: prefix.into(),
            }));
        }
        Ok(resident
            .indexes
            .get(index)
            .into_iter()
            .flat_map(|entries| entries.iter())
            .filter(|(key, _)| key.starts_with(prefix))
            .flat_map(|(_, keys)| keys.iter().map(|key| resident.rows[key].clone()))
            .collect())
    }

    /// A complete insert requires no resident read. Database constraints validate
    /// cold-key uniqueness at commit. Reads of this staged primary key are hits.
    pub fn insert(&mut self, table: &str, row: Row) -> Result<(), Error> {
        self.check(|this| {
            let schema = this.catalog.table(table)?;
            schema.validate_row(&row)?;
            let key = schema.key(&row);
            let resident = this.residents.get_mut(table).ok_or(Error::Invalid)?;
            if resident.rows.contains_key(&key) {
                return Err(Error::Constraint);
            }
            resident.absent.remove(&key);
            resident.rows.insert(key, row.clone());
            resident.reindex(schema);
            this.writes.push(Write::Insert {
                table: table.into(),
                row,
            });
            Ok(())
        })
    }

    pub fn update(&mut self, table: &str, key: &[Value], changes: Row) -> Result<(), Error> {
        self.check(|this| {
            let mut row = this.get(table, key)?.ok_or(Error::NotFound)?;
            let schema = this.catalog.table(table)?;
            if changes.keys().any(|c| schema.primary.contains(c)) {
                return Err(Error::Invalid);
            }
            row.extend(changes);
            schema.validate_row(&row)?;
            let resident = this.residents.get_mut(table).ok_or(Error::Invalid)?;
            resident.rows.insert(key.into(), row.clone());
            resident.reindex(schema);
            this.writes.push(Write::Update {
                table: table.into(),
                key: key.into(),
                row,
            });
            Ok(())
        })
    }

    pub fn delete(&mut self, table: &str, key: &[Value]) -> Result<bool, Error> {
        self.check(|this| {
            let Some(_) = this.get(table, key)? else {
                return Ok(false);
            };
            let schema = this.catalog.table(table)?;
            let resident = this.residents.get_mut(table).ok_or(Error::Invalid)?;
            resident.rows.remove(key);
            resident.absent.insert(key.into());
            resident.reindex(schema);
            this.writes.push(Write::Delete {
                table: table.into(),
                key: key.into(),
            });
            Ok(true)
        })
    }
}
