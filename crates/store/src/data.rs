//! Residency owned by a data interface. Operations select interfaces; only their
//! implementations know storage names. Hosts prepare them before transaction entry.
use crate::{Backend, Error, Store};
use alloc::vec::Vec;

#[derive(Clone, Default)]
pub struct Data {
    tables: Vec<&'static str>,
}
impl Data {
    pub fn new(tables: &'static [&'static str]) -> Self {
        Self {
            tables: tables.to_vec(),
        }
    }
    pub fn and(mut self, other: Self) -> Self {
        for table in other.tables {
            if !self.tables.contains(&table) {
                self.tables.push(table);
            }
        }
        self
    }
    pub fn prepare<B: Backend>(&self, store: &mut Store<B>) -> Result<(), Error> {
        for table in &self.tables {
            store.load(table)?;
        }
        Ok(())
    }
    pub fn contains(&self, table: &str) -> bool {
        self.tables.contains(&table)
    }
}
