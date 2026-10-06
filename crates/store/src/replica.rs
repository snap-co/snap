//! Ordered, volatile authoritative replicas. Only server publications belong here;
//! clients send operation arguments to the authority, never these programs.
use crate::{Catalog, Error, Program, Row, Store, Value, memory::Memory};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// The binary program is transported as bytes inside the current JSON carrier.
/// Sequence numbers belong to one physical subscription, not the durable log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    pub sequence: u64,
    pub reset: bool,
    pub program: Vec<u8>,
}

pub struct Replica {
    store: Store<Memory>,
    sequence: Option<u64>,
}

impl Replica {
    pub fn new(catalog: Catalog) -> Result<Self, Error> {
        let backend = Memory::new(catalog.clone())?;
        let mut store = Store::new(catalog, backend)?;
        let names: Vec<_> = store
            .catalog()
            .tables
            .iter()
            .map(|t| t.name.clone())
            .collect();
        for name in names {
            store.load(&name)?;
        }
        Ok(Self {
            store,
            sequence: None,
        })
    }

    /// Snapshots can jump over redacted/compacted queued frames. Incremental
    /// programs must be consecutive. Duplicates are ignored only when they are
    /// at/before the applied position in this attachment. Reconnect starts anew.
    /// Invalid input or execution failure never replaces the current replica.
    pub fn apply(&mut self, publication: &Publication) -> Result<bool, Error> {
        if publication.sequence == 0 {
            return Err(Error::Invalid);
        }
        if self.sequence.is_some_and(|n| publication.sequence <= n) {
            return Ok(false);
        }
        let program = Program::from_bytes(self.store.catalog(), &publication.program)?;
        if publication.reset {
            let mut fresh = Self::new(self.store.catalog().clone())?;
            fresh.store.replay(&program)?;
            fresh.sequence = Some(publication.sequence);
            *self = fresh;
        } else {
            if self.sequence.and_then(|n| n.checked_add(1)) != Some(publication.sequence) {
                return Err(Error::Invalid);
            }
            self.store.replay(&program)?;
            self.sequence = Some(publication.sequence);
        }
        Ok(true)
    }

    pub fn get(&mut self, table: &str, key: &[Value]) -> Result<Option<Row>, Error> {
        self.store.inspect("replica.read", |tx| tx.get(table, key))
    }
    pub fn sequence(&self) -> Option<u64> {
        self.sequence
    }
}
