//! Volatile Store backend for resident clients. It has no IO or restart durability.
//! It validates constraints on a private copy before publishing an entire program.
use crate::{Backend, Catalog, CommitError, Error, Instruction, Program, Row, Rows, Table, Value};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
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
            .map(|t| (t.name.clone(), BTreeMap::new()))
            .collect();
        Ok(Self { catalog, tables })
    }

    fn apply(&self, tables: &mut Tables, program: &Program) -> Result<(), Error> {
        program.validate_schema(&self.catalog)?;
        for instruction in program.instructions() {
            let schema = self.catalog.table(instruction.table())?;
            let rows = tables.get_mut(&schema.name).ok_or(Error::Invalid)?;
            match instruction {
                Instruction::Insert { row, .. } => {
                    if rows.insert(schema.key(&row), row).is_some() {
                        return Err(Error::Constraint);
                    }
                }
                Instruction::Update { key, changes, .. } => {
                    rows.get_mut(&key).ok_or(Error::Constraint)?.extend(changes);
                }
                Instruction::Delete { key, .. } => {
                    if rows.remove(&key).is_none() {
                        return Err(Error::Constraint);
                    }
                }
            }
            // Unique indexes are immediate; foreign keys are deferred to commit.
            for index in schema.indexes.iter().filter(|index| index.unique) {
                let mut seen = BTreeSet::new();
                for row in rows.values() {
                    let key: Vec<_> = index.columns.iter().map(|c| row[c].clone()).collect();
                    if !seen.insert(key) {
                        return Err(Error::Constraint);
                    }
                }
            }
        }
        for schema in &self.catalog.tables {
            for row in tables[&schema.name].values() {
                schema.validate_row(row)?;
                for foreign in &schema.foreign {
                    let key: Vec<_> = foreign.columns.iter().map(|c| row[c].clone()).collect();
                    if !tables[&foreign.table].contains_key(&key) {
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
        Ok(self
            .tables
            .get(&table.name)
            .ok_or(Error::Invalid)?
            .values()
            .cloned()
            .collect())
    }
    fn commit(&mut self, program: &Program) -> Result<(), CommitError> {
        let mut staged = self.tables.clone();
        self.apply(&mut staged, program)
            .map_err(CommitError::Rejected)?;
        self.tables = staged;
        Ok(())
    }
}
