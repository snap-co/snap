//! Controlled Store backend for testing platforms. It owns its rows, with no IO
//! or persistence across destruction. A commit publishes a complete private copy
//! only after validation; foreign keys are checked at the transaction boundary.
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};
use snap_store::{
    Backend, Catalog, CommitError, Error, Instruction, Program, Row, Rows, Table, Value,
};

type Tables = BTreeMap<String, BTreeMap<Vec<Value>, Row>>;

pub struct Memory {
    catalog: Catalog,
    tables: Tables,
}

impl Memory {
    pub fn new(catalog: Catalog) -> Result<Self, Error> {
        catalog.validate()?;
        let tables = catalog
            .tables
            .iter()
            .map(|table| (table.name.clone(), BTreeMap::new()))
            .collect();
        Ok(Self { catalog, tables })
    }

    fn apply(&self, tables: &mut Tables, program: &Program) -> Result<(), Error> {
        program.validate_schema(&self.catalog)?;
        for instruction in program.instructions() {
            let name = instruction.table();
            let schema = self.catalog.table(name)?;
            let rows = tables.get_mut(name).ok_or(Error::Invalid)?;
            match &instruction {
                Instruction::Insert { row, .. } => {
                    schema.validate_row(row)?;
                    if rows.insert(schema.key(row), row.clone()).is_some() {
                        return Err(Error::Constraint);
                    }
                }
                Instruction::Update { key, changes, .. } => {
                    let row = rows.get_mut(key).ok_or(Error::Constraint)?;
                    row.extend(changes.clone());
                    schema.validate_row(row)?;
                }
                Instruction::Delete { key, .. } => {
                    if rows.remove(key).is_none() {
                        return Err(Error::Constraint);
                    }
                }
            }
            for index in schema.indexes.iter().filter(|index| index.unique) {
                let mut values = BTreeSet::new();
                for row in rows.values() {
                    let key: Vec<_> = index
                        .columns
                        .iter()
                        .map(|column| row[column].clone())
                        .collect();
                    if !values.insert(key) {
                        return Err(Error::Constraint);
                    }
                }
            }
        }
        for schema in &self.catalog.tables {
            for foreign in &schema.foreign {
                let target = &tables[&foreign.table];
                for row in tables[&schema.name].values() {
                    let key: Vec<_> = foreign
                        .columns
                        .iter()
                        .map(|column| row[column].clone())
                        .collect();
                    if !target.contains_key(&key) {
                        return Err(Error::Constraint);
                    }
                }
            }
        }
        Ok(())
    }
}

impl Backend for Memory {
    fn load(&mut self, table: &Table) -> Result<Rows, Error> {
        if self.catalog.table(&table.name)? != table {
            return Err(Error::Invalid);
        }
        Ok(self.tables[&table.name].values().cloned().collect())
    }

    fn commit(&mut self, program: &Program) -> Result<(), CommitError> {
        let mut staged = self.tables.clone();
        self.apply(&mut staged, program)
            .map_err(CommitError::Rejected)?;
        self.tables = staged;
        Ok(())
    }
}

/// Dependency fault control, not a replacement dispatcher. Reject exactly the
/// next nonempty commit before forwarding any writes. Loads and later commits
/// still exercise the wrapped backend. This models confirmed rollback only,
/// never an indeterminate outcome or a process crash.
pub struct RejectOnce<B> {
    backend: B,
    reject: Arc<AtomicBool>,
}
pub struct CommitRejection(Arc<AtomicBool>);
impl CommitRejection {
    pub fn arm(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl<B> RejectOnce<B> {
    pub fn new(backend: B) -> (Self, CommitRejection) {
        let reject = Arc::new(AtomicBool::new(false));
        (
            Self {
                backend,
                reject: reject.clone(),
            },
            CommitRejection(reject),
        )
    }
}
impl<B: Backend> Backend for RejectOnce<B> {
    fn load(&mut self, table: &Table) -> Result<Rows, Error> {
        self.backend.load(table)
    }
    fn commit(&mut self, program: &Program) -> Result<(), CommitError> {
        if !program.is_empty() && self.reject.swap(false, Ordering::SeqCst) {
            return Err(CommitError::Rejected(Error::Unavailable));
        }
        self.backend.commit(program)
    }
}
