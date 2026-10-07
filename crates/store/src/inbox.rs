//! Bounded, byte-charged handoff between a platform carrier and application code.
//!
//! Each direction has its own queue and budget. Draining releases an item's
//! charge, so the budget bounds queued bytes rather than lifetime traffic.
use alloc::collections::VecDeque;
use spin::Mutex;

/// Why a producer could not enqueue. The item comes back with the error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// No free slot, or the item's size exceeds the remaining byte budget.
    Full,
}

/// A fixed-capacity FIFO that supports concurrent producers and consumers.
///
/// A short spin-lock protects both storage and byte reservations. Budget checks,
/// insertion and removal are serialized together, so reservations cannot be lost
/// or exceeded by concurrent callers. No caller code or item destructor runs
/// under the lock. This queue is not lock-free and must not be called from an
/// interrupt that can preempt a caller holding the lock.
pub struct Channel<T> {
    state: Mutex<State<T>>,
    slots: usize,
    budget: usize,
}

struct State<T> {
    items: VecDeque<(T, usize)>,
    charged: usize,
}

impl<T> Channel<T> {
    /// `slots` is rounded up to a power of two, with a minimum of one.
    pub fn new(slots: usize, budget: usize) -> Self {
        let slots = slots.max(1).next_power_of_two();
        Self {
            state: Mutex::new(State {
                items: VecDeque::with_capacity(slots),
                charged: 0,
            }),
            slots,
            budget,
        }
    }

    /// Enqueue an item, reserving its decoded wire size until it is drained.
    pub fn push(&self, value: T, bytes: usize) -> Result<(), (T, Rejected)> {
        let mut state = self.state.lock();
        if state.items.len() == self.slots || bytes > self.budget - state.charged {
            return Err((value, Rejected::Full));
        }
        state.items.push_back((value, bytes));
        state.charged += bytes;
        Ok(())
    }

    /// Take ownership of the oldest item and release its byte reservation.
    pub fn pop(&self) -> Option<(T, usize)> {
        let mut state = self.state.lock();
        let item = state.items.pop_front()?;
        state.charged -= item.1;
        Some(item)
    }

    /// Occupancy at the instant the queue is locked; other callers may change it.
    pub fn len(&self) -> usize {
        self.state.lock().items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.slots
    }

    pub fn budget(&self) -> usize {
        self.budget
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for Channel<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Channel")
            .field("len", &self.len())
            .field("capacity", &self.slots)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}
