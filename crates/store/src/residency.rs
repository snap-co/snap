//! Union of logical-reader requirements, accepted-work pins and cleanup ownership.
//! Hosts update references and drive loading; Store owns retention mechanics.
use crate::{Backend, Data, Error, Store, Value, resource};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

#[derive(Default)]
pub struct Residency {
    tables: BTreeMap<String, Requirements>,
}
#[derive(Default)]
struct Requirements {
    readers: BTreeMap<String, BTreeSet<Vec<Value>>>,
    work: BTreeSet<Vec<Value>>,
}
impl Residency {
    pub fn set(&mut self, table: &str, reader: String, keys: BTreeSet<Vec<Value>>) {
        self.tables
            .entry(table.into())
            .or_default()
            .readers
            .insert(reader, keys);
    }
    pub fn remove(&mut self, table: &str, reader: &str) {
        if let Some(table) = self.tables.get_mut(table) {
            table.readers.remove(reader);
        }
    }
    pub fn pin(&mut self, resource: &resource::Resource) {
        self.tables
            .entry(resource.table.clone())
            .or_default()
            .work
            .insert(resource.key.clone());
    }
    pub fn pin_reader(&mut self, table: &str, reader: &str) {
        if let Some(table) = self.tables.get_mut(table)
            && let Some(keys) = table.readers.get(reader)
        {
            table.work.extend(keys.iter().cloned());
        }
    }
    pub fn release_work(&mut self) {
        for table in self.tables.values_mut() {
            table.work.clear();
        }
    }
    pub fn references(&self, resource: &resource::Resource) -> usize {
        self.tables.get(&resource.table).map_or(0, |table| {
            table
                .readers
                .values()
                .filter(|keys| keys.contains(&resource.key))
                .count()
        })
    }
    /// Never narrow a table currently required in full by the FIFO owner.
    pub fn apply<B: Backend>(&self, store: &mut Store<B>, full: &Data) -> Result<(), Error> {
        for (name, requirements) in &self.tables {
            let mut keys: BTreeSet<_> = requirements
                .readers
                .values()
                .flat_map(|keys| keys.iter())
                .chain(requirements.work.iter())
                .cloned()
                .collect();
            keys.extend(store.inspect("residency.cleanup", |tx| resource::cleanup_keys(tx, name))?);
            store.load_keys(name, &keys)?;
            if !full.contains(name) {
                store.retain_keys(name, &keys)?;
            }
        }
        Ok(())
    }
}
