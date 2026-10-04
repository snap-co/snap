//! Persisted lifecycle and cleanup ownership for rows of any module. Keys include
//! the owning table and its complete primary key. Mutations compose in the caller's
//! transaction; this module neither dispatches operations nor executes controllers.
use crate::{Data, Error, Row, RowChange, Transaction, Value};
use alloc::{collections::BTreeSet, string::String, vec, vec::Vec};
use serde::{Deserialize, Serialize};

pub const MIGRATION: &str = include_str!("../migrations/0000_store_resources.toml");
pub const TABLE: &str = "store.resources";
pub fn data() -> Data {
    Data::new(&[TABLE])
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Resource {
    pub table: String,
    pub key: Vec<Value>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Active,
    Deleted,
    Archived,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lifecycle {
    pub state: State,
    pub finalizers: BTreeSet<String>,
    pub blocked: Option<String>,
}
impl Resource {
    pub fn new(table: &str, key: &[Value]) -> Self {
        Self {
            table: table.into(),
            key: key.to_vec(),
        }
    }
    fn storage_key(&self) -> Result<Vec<Value>, Error> {
        if self.table.is_empty() || self.key.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(vec![
            self.table.clone().into(),
            serde_json::to_string(&self.key)
                .map_err(|_| Error::Invalid)?
                .into(),
        ])
    }
    pub fn lifecycle(&self, tx: &mut Transaction<'_>) -> Result<Lifecycle, Error> {
        let Some(row) = tx.get(TABLE, &self.storage_key()?)? else {
            return Ok(Lifecycle::default());
        };
        fn decode<T: serde::de::DeserializeOwned>(row: &Row, name: &str) -> Result<T, Error> {
            let Some(Value::Text(text)) = row.get(name) else {
                return Err(Error::Invalid);
            };
            serde_json::from_str(text).map_err(|_| Error::Invalid)
        }
        Ok(Lifecycle {
            state: decode(&row, "state")?,
            finalizers: decode(&row, "finalizers")?,
            blocked: decode(&row, "blocked")?,
        })
    }
    pub fn set_lifecycle(
        &self,
        tx: &mut Transaction<'_>,
        lifecycle: &Lifecycle,
    ) -> Result<(), Error> {
        if self.lifecycle(tx)? == *lifecycle {
            return Ok(());
        }
        let key = self.storage_key()?;
        let mut row = Row::from([
            (
                "state".into(),
                serde_json::to_string(&lifecycle.state)
                    .map_err(|_| Error::Invalid)?
                    .into(),
            ),
            (
                "finalizers".into(),
                serde_json::to_string(&lifecycle.finalizers)
                    .map_err(|_| Error::Invalid)?
                    .into(),
            ),
            (
                "blocked".into(),
                serde_json::to_string(&lifecycle.blocked)
                    .map_err(|_| Error::Invalid)?
                    .into(),
            ),
        ]);
        if tx.get(TABLE, &key)?.is_some() {
            tx.update(TABLE, &key, row)
        } else {
            row.insert("table".into(), key[0].clone());
            row.insert("key".into(), key[1].clone());
            tx.insert(TABLE, row)
        }
    }
    pub fn finalize(&self, tx: &mut Transaction<'_>, finalizer: &str) -> Result<(), Error> {
        let mut lifecycle = self.lifecycle(tx)?;
        lifecycle.finalizers.remove(finalizer);
        self.set_lifecycle(tx, &lifecycle)
    }
    pub fn retry(&self, tx: &mut Transaction<'_>) -> Result<(), Error> {
        let mut lifecycle = self.lifecycle(tx)?;
        lifecycle.blocked = None;
        self.set_lifecycle(tx, &lifecycle)
    }
    /// Interpret generic lifecycle notifications without knowing the owning module.
    pub fn changed(change: &RowChange) -> Result<Self, Error> {
        if change.table != TABLE {
            return Ok(Self::new(&change.table, &change.key));
        }
        let [Value::Text(table), Value::Text(key)] = change.key.as_slice() else {
            return Err(Error::Invalid);
        };
        Ok(Self {
            table: table.clone(),
            key: serde_json::from_str(key).map_err(|_| Error::Invalid)?,
        })
    }
}

/// Rows with unfinished cleanup retain residency even without a reader. Metadata
/// is independent of value residency and supports arbitrary composite keys.
pub fn cleanup_keys(tx: &mut Transaction<'_>, table: &str) -> Result<BTreeSet<Vec<Value>>, Error> {
    let mut keys = BTreeSet::new();
    for row in tx.find(TABLE, "primary", &[table.into()])? {
        let Some(Value::Text(key)) = row.get("key") else {
            return Err(Error::Invalid);
        };
        let key = serde_json::from_str(key).map_err(|_| Error::Invalid)?;
        let resource = Resource {
            table: table.into(),
            key,
        };
        if !resource.lifecycle(tx)?.finalizers.is_empty() {
            keys.insert(resource.key);
        }
    }
    Ok(keys)
}
