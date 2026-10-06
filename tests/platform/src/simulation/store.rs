use super::{Action, Timeline};
use alloc::rc::Rc;
use core::cell::Cell;
use snap_store::{Backend, Catalog, CommitError, Error, Program, Rows, Table, memory::Memory};

/// Simulated Store IO over the production volatile program interpreter. This is
/// not an independent oracle or a SQLite emulator. Snap's real Store still owns
/// transactions, residency, caught-MISS poisoning and publication.
///
/// Backend is synchronous: latency advances virtual time while the whole host
/// step is blocked. Other events cannot interleave inside load/commit. Success
/// means atomic application in this volatile authority, not fsync durability.
pub struct Store {
    backend: Memory,
    timeline: Timeline,
    reject: Rc<Cell<bool>>,
}
#[derive(Clone)]
pub struct CommitFault(Rc<Cell<bool>>);
impl CommitFault {
    /// Confirmed rejection before applying the next nonempty program. No retry.
    pub fn reject_next(&self) {
        self.0.set(true);
    }
}
impl Store {
    pub fn new(catalog: Catalog, timeline: Timeline) -> Result<Self, Error> {
        Ok(Self {
            backend: Memory::new(catalog)?,
            timeline,
            reject: Rc::new(Cell::new(false)),
        })
    }
    pub fn faults(&self) -> CommitFault {
        CommitFault(self.reject.clone())
    }
}
impl Backend for Store {
    fn load(&mut self, table: &Table) -> Result<Rows, Error> {
        self.timeline.elapse(self.timeline.schedule().load_ms);
        let rows = self.backend.load(table);
        self.timeline.record(Action::StoreLoad {
            table: table.name.clone(),
        });
        rows
    }
    fn commit(&mut self, program: &Program) -> Result<(), CommitError> {
        self.timeline.elapse(self.timeline.schedule().commit_ms);
        let rejected = !program.is_empty() && self.reject.replace(false);
        let result = if rejected {
            Err(CommitError::Rejected(Error::Unavailable))
        } else {
            self.backend.commit(program)
        };
        self.timeline.record(Action::StoreCommit {
            rejected: result.is_err(),
        });
        result
    }
}
