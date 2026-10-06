//! Production volatile Store plus controlled commit rejection for platform tests.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
pub use snap_store::memory::Memory;
use snap_store::{Backend, CommitError, Error, Program, Rows, Table};

/// Reject exactly the next nonempty commit before forwarding any writes. Loads
/// and later commits still exercise the wrapped backend. Confirmed rollback only.
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
