//! Controlled Store backend for testing platforms. It owns its rows, with no IO
//! or persistence across destruction. A commit publishes a complete private copy
//! only after validation; foreign keys are checked at the transaction boundary.
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};
use snap_store::{Backend, Catalog, CommitError, Error, Row, Rows, Table, Value, Write};

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

    fn apply(&self, tables: &mut Tables, writes: &[Write]) -> Result<(), Error> {
        for write in writes {
            let name = match write {
                Write::Insert { table, .. }
                | Write::Update { table, .. }
                | Write::Delete { table, .. } => table,
            };
            let schema = self.catalog.table(name)?;
            let rows = tables.get_mut(name).ok_or(Error::Invalid)?;
            match write {
                Write::Insert { row, .. } => {
                    schema.validate_row(row)?;
                    if rows.insert(schema.key(row), row.clone()).is_some() {
                        return Err(Error::Constraint);
                    }
                }
                Write::Update { key, row, .. } => {
                    schema.validate_row(row)?;
                    if rows.remove(key).is_none() {
                        return Err(Error::Constraint);
                    }
                    if rows.insert(schema.key(row), row.clone()).is_some() {
                        return Err(Error::Constraint);
                    }
                }
                Write::Delete { key, .. } => {
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

    fn commit(&mut self, writes: &[Write]) -> Result<(), CommitError> {
        let mut staged = self.tables.clone();
        self.apply(&mut staged, writes)
            .map_err(CommitError::Rejected)?;
        self.tables = staged;
        Ok(())
    }
}
