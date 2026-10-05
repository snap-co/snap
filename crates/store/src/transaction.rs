//! Server-side resident transactions. Handlers receive only `Transaction`, never
//! a backend. A miss poisons the attempt, even if its Result is caught. There is
//! no suspension or implicit retry. Hosts explicitly load, then callers may retry.
//! This is cooperative IO isolation, not a sandbox for arbitrary Rust callbacks.
use crate::{Catalog, Instruction, Program, Row, Rows, Table, Value, kind};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

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

pub enum CommitError {
    /// Backend guarantees no write from this transaction committed.
    Rejected(Error),
    Indeterminate,
}

/// Host IO seam. The backend owns exclusive write authority for its lifetime.
/// `load` returns the COMPLETE table from that authority, never a partial page.
/// `commit` atomically executes the binary program, in order, before returning
/// success. It must reject programs for another schema. A durable program log,
/// when supplied, commits in the same transaction as its materialized data.
/// Failure must distinguish confirmed rollback from an unknown commit outcome.
/// No other connection, process, or out-of-band writer may modify the database
/// while resident data is served. The SQLite adapter enforces an exclusive lock.
pub trait Backend {
    fn load(&mut self, table: &Table) -> Result<Rows, Error>;
    fn commit(&mut self, program: &Program) -> Result<(), CommitError>;
}

/// Host access to private committed programs. Positions belong to this log,
/// not a cluster-wide ordering. Programs can contain secret material.
pub trait ProgramLog: Backend {
    /// Return at most `limit` entries in ascending position, strictly after
    /// `after`. No handler or controller is invoked while reading the log.
    fn programs(&mut self, after: u64, limit: usize) -> Result<Vec<LoggedProgram>, Error>;
}

#[derive(Debug)]
pub struct LoggedProgram {
    pub position: u64,
    pub program: Program,
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
    /// The exact committed instructions, before client authorization/filtering.
    /// Empty for read-only transactions. Never broadcast this wholesale.
    pub program: Program,
    /// Net row changes, available only after successful commit and publication.
    /// This is an in-process notification, not a durable delivery log.
    pub changes: Vec<RowChange>,
}

/// One changed primary key. Multiple writes in a transaction coalesce; a net
/// no-op produces no change. A successful cold insert establishes prior absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowChange {
    pub table: String,
    pub key: Vec<Value>,
    pub before: Option<Row>,
    pub after: Option<Row>,
}

pub struct Store<B> {
    catalog: Catalog,
    schema_id: [u8; 32],
    backend: B,
    residents: BTreeMap<String, Resident>,
    misses: Misses,
    fenced: bool,
}

impl<B: Backend> Store<B> {
    pub fn new(catalog: Catalog, backend: B) -> Result<Self, Error> {
        catalog.validate()?;
        let schema_id = crate::program::schema_id(&catalog)?;
        Ok(Self {
            schema_id,
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

    /// Resident-only observation/admission. A callback that stages any write is
    /// rejected and rolled back; this entry point never calls backend IO.
    pub fn inspect<T>(
        &mut self,
        operation: &str,
        read: impl FnOnce(&mut Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.run(operation, |tx| {
            let value = read(tx)?;
            if !tx.program.is_empty() {
                return Err(Error::Invalid);
            }
            Ok(value)
        })
        .map(|committed| committed.value)
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

    /// Explicit residency IO for primary keys. The initial backend interface loads
    /// a table temporarily; only requested rows and known absences remain resident.
    /// This does not establish complete secondary-index knowledge.
    pub fn load_keys(&mut self, table: &str, keys: &BTreeSet<Vec<Value>>) -> Result<(), Error> {
        if self.fenced {
            return Err(Error::Indeterminate);
        }
        let schema = self.catalog.table(table)?;
        for key in keys {
            if key.len() != schema.primary.len()
                || schema.primary.iter().zip(key).any(|(column, value)| {
                    schema.column(column).map(|column| column.kind) != Ok(kind(value))
                })
            {
                return Err(Error::Invalid);
            }
        }
        let resident = &self.residents[table];
        let missing: BTreeSet<_> = keys
            .iter()
            .filter(|key| {
                !resident.complete
                    && !resident.rows.contains_key(*key)
                    && !resident.absent.contains(*key)
            })
            .cloned()
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let loaded = self.backend.load(schema)?;
        let mut rows = BTreeMap::new();
        for row in loaded {
            schema.validate_row(&row)?;
            if rows.insert(schema.key(&row), row).is_some() {
                return Err(Error::Invalid);
            }
        }
        let resident = self.residents.get_mut(table).ok_or(Error::Invalid)?;
        for key in missing {
            if let Some(row) = rows.remove(&key) {
                resident.rows.insert(key, row);
            } else {
                resident.absent.insert(key);
            }
        }
        resident.reindex(schema);
        Ok(())
    }

    /// Release unreferenced resident knowledge, never persistent rows. The host
    /// must include accepted work and draining logical connections in `keys`.
    pub fn retain_keys(&mut self, table: &str, keys: &BTreeSet<Vec<Value>>) -> Result<(), Error> {
        if self.fenced {
            return Err(Error::Indeterminate);
        }
        let schema = self.catalog.table(table)?;
        let resident = self.residents.get_mut(table).ok_or(Error::Invalid)?;
        if resident.complete {
            for key in keys {
                if !resident.rows.contains_key(key) {
                    resident.absent.insert(key.clone());
                }
            }
        }
        resident.rows.retain(|key, _| keys.contains(key));
        resident.absent.retain(|key| keys.contains(key));
        resident.complete = false;
        resident.reindex(schema);
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
            program: Program::empty(self.schema_id),
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
        // Allocate the feed before commit, just like resident indexes. Observers
        // receive it only after the backend confirms the entire transaction.
        let keys: BTreeSet<_> = tx
            .program
            .instructions()
            .map(|instruction| match instruction {
                Instruction::Insert { table, row } => (
                    table.clone(),
                    self.catalog
                        .table(&table)
                        .expect("validated table")
                        .key(&row),
                ),
                Instruction::Update { table, key, .. } | Instruction::Delete { table, key } => {
                    (table, key)
                }
            })
            .collect();
        let changes = keys
            .into_iter()
            .filter_map(|(table, key)| {
                let before = self.residents[&table].rows.get(&key).cloned();
                let after = tx.residents[&table].rows.get(&key).cloned();
                (before != after).then_some(RowChange {
                    table,
                    key,
                    before,
                    after,
                })
            })
            .collect();
        if !tx.program.is_empty() {
            // Also fence unwinding out of a host commit: its effects may already
            // be durable even though the host never returned a classified result.
            self.fenced = true;
            match self.backend.commit(&tx.program) {
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
        Ok(Committed {
            value,
            changes,
            program: tx.program,
        })
    }

    /// Apply a trusted program without the original handler. Hosts must prepare
    /// resident data first, supply the matching checkpoint/schema, and apply
    /// each committed position once, in order. This is not duplicate suppression
    /// or concurrent-snapshot validation. Replaying commits through this Store's
    /// backend and produces its normal post-commit change feed. It does not run
    /// application handlers, controllers or their external effects.
    pub fn replay(&mut self, program: &Program) -> Result<Committed<()>, Error> {
        if self.fenced {
            return Err(Error::Indeterminate);
        }
        if !program.matches(&self.schema_id) {
            return Err(Error::Invalid);
        }
        self.run("store.replay", |tx| {
            tx.program = program.clone();
            for instruction in program.instructions() {
                tx.apply(instruction)?;
            }
            Ok(())
        })
    }
}

impl<B: ProgramLog> Store<B> {
    pub fn programs(&mut self, after: u64, limit: usize) -> Result<Vec<LoggedProgram>, Error> {
        if self.fenced {
            return Err(Error::Indeterminate);
        }
        if limit == 0 || limit > 4096 {
            return Err(Error::Invalid);
        }
        self.backend.programs(after, limit)
    }
}

pub struct Transaction<'a> {
    catalog: &'a Catalog,
    residents: BTreeMap<String, Resident>,
    program: Program,
    failed: Option<Error>,
}

impl Transaction<'_> {
    /// Return a previous Store failure even if its caller caught the result.
    /// Platforms check this before validating handler output so a poisoned
    /// transaction remains a storage failure, not an output-schema rejection.
    /// Checking status neither clears the failure nor performs backend IO.
    pub fn status(&self) -> Result<(), Error> {
        match &self.failed {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

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
        self.emit(Instruction::Insert {
            table: table.into(),
            row,
        })
    }

    pub fn update(&mut self, table: &str, key: &[Value], changes: Row) -> Result<(), Error> {
        self.check(|this| {
            if changes.is_empty() {
                this.get(table, key)?.ok_or(Error::NotFound)?;
                return Ok(());
            }
            this.emit(Instruction::Update {
                table: table.into(),
                key: key.into(),
                changes,
            })
        })
    }

    pub fn delete(&mut self, table: &str, key: &[Value]) -> Result<bool, Error> {
        self.check(|this| {
            let Some(_) = this.get(table, key)? else {
                return Ok(false);
            };
            this.emit(Instruction::Delete {
                table: table.into(),
                key: key.into(),
            })?;
            Ok(true)
        })
    }

    // Authoring and replay use this one executor. Append before applying so an
    // encoding failure poisons/discards the attempt just like a state failure.
    fn emit(&mut self, instruction: Instruction) -> Result<(), Error> {
        self.check(|this| {
            instruction.validate(this.catalog)?;
            let instruction = this.program.push(instruction)?;
            this.apply(instruction)
        })
    }

    fn apply(&mut self, instruction: Instruction) -> Result<(), Error> {
        self.check(|this| {
            instruction.validate(this.catalog)?;
            let table: String = instruction.table().into();
            let catalog = this.catalog;
            let schema = catalog.table(&table)?;
            match instruction {
                Instruction::Insert { row, .. } => {
                    let key = schema.key(&row);
                    let resident = this.residents.get_mut(&table).ok_or(Error::Invalid)?;
                    if resident.rows.contains_key(&key) {
                        return Err(Error::Constraint);
                    }
                    resident.absent.remove(&key);
                    resident.rows.insert(key, row);
                }
                Instruction::Update { key, changes, .. } => {
                    let mut row = this.get(&table, &key)?.ok_or(Error::NotFound)?;
                    row.extend(changes);
                    schema.validate_row(&row)?;
                    this.residents
                        .get_mut(&table)
                        .ok_or(Error::Invalid)?
                        .rows
                        .insert(key, row);
                }
                Instruction::Delete { key, .. } => {
                    this.get(&table, &key)?.ok_or(Error::NotFound)?;
                    let resident = this.residents.get_mut(&table).ok_or(Error::Invalid)?;
                    resident.rows.remove(&key);
                    resident.absent.insert(key);
                }
            }
            this.residents
                .get_mut(&table)
                .ok_or(Error::Invalid)?
                .reindex(schema);
            Ok(())
        })
    }
}
