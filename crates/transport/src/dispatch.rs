//! Application-wide scheduling. Carriers enqueue work; only the current FIFO
//! owner may prepare, admit and execute it. Output buffers are not operation queues.
use alloc::collections::{VecDeque, vec_deque};

/// One serialized operation lane. Acquiring a slot holds the gate even while
/// residency or acceptance is being prepared, and after the work has been moved
/// into its handler. The owner releases it only after completion or rejection.
/// The host must drive the lane under exclusion; this type creates no threads.
pub struct Queue<T> {
    pending: VecDeque<T>,
    busy: bool,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            busy: false,
        }
    }
}

impl<T> Queue<T> {
    pub fn push_back(&mut self, work: T) {
        self.pending.push_back(work);
    }

    /// Nothing else can acquire the lane until the owner calls `finish`.
    pub fn acquire(&mut self) -> Option<T> {
        if self.busy {
            return None;
        }
        let work = self.pending.pop_front()?;
        self.busy = true;
        Some(work)
    }

    pub fn finish(&mut self) {
        assert!(self.busy, "only the active operation may release the lane");
        self.busy = false;
    }

    pub fn busy(&self) -> bool {
        self.busy
    }
    pub fn len(&self) -> usize {
        self.pending.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
    pub fn idle(&self) -> bool {
        !self.busy && self.pending.is_empty()
    }
    pub fn iter(&self) -> vec_deque::Iter<'_, T> {
        self.pending.iter()
    }
    pub fn iter_mut(&mut self) -> vec_deque::IterMut<'_, T> {
        self.pending.iter_mut()
    }
    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.pending.retain(keep);
    }
    /// Retired, unaccepted work may deliver a terminal rejection while another
    /// operation holds the lane. Removing it does not grant execution authority.
    pub fn remove(&mut self, index: usize) -> Option<T> {
        self.pending.remove(index)
    }
}
