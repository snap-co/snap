//! Bounded single-producer handoff between a platform carrier and application code.
//!
//! A carrier decodes wire bytes and publishes an item; the application drains it
//! on its own thread and publishes a reply in the other direction. Neither side
//! executes the other's code, and neither holds the other's state, so this stays
//! platform-agnostic: the same queue serves a browser WebSocket, a TLS socket and
//! an in-memory test carrier.
//!
//! The queue *is* the synchronization. A ring with release/acquire cursors needs
//! no lock, so the application half never has to be `Sync` and a carrier can never
//! reach into application state.
//!
//! Each direction has exactly one producer and one consumer:
//!
//! - **requests** — produced by the carrier, consumed by the application
//! - **replies** — produced by the application, consumed by the carrier
//!
//! Both are byte-charged. `open` on a carrier interface supplies the budget, so a
//! carrier that outruns the application is refused rather than allowed to grow the
//! queue without limit. The charge is released on drain, making the budget a live
//! window rather than a lifetime total.
use alloc::vec::Vec;
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Why a producer could not enqueue. The item comes back with the error, so a
/// caller never loses a decoded value to a refused queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// No free slot, or the item's size exceeds the remaining byte budget.
    Full,
}

/// One direction of a handoff: exactly one producer, exactly one consumer.
///
/// Slots are reused, so the backing storage is fixed for the queue's lifetime.
/// The producer may only write a slot the consumer's cursor has passed, and the
/// consumer may only read a slot the producer's cursor has published. Neither
/// invariant is expressible in the borrow checker, so both are upheld by the
/// cursor ordering below and are `unsafe` to assert.
pub struct Channel<T> {
    /// Each slot carries its own reservation, so a drained item releases exactly
    /// what its producer charged rather than a share of a running total.
    items: UnsafeCell<Vec<MaybeUninit<(T, usize)>>>,
    /// Next index the producer will fill.
    head: AtomicUsize,
    /// Next index the consumer will take.
    tail: AtomicUsize,
    mask: usize,
    slots: usize,
    budget: usize,
    /// Bytes currently reserved. Only the producer raises it and only the consumer
    /// lowers it, so a relaxed atomic is sufficient.
    charged: AtomicUsize,
}

// Safety: each slot is written by the producer alone and read by the consumer
// alone, in that order, and never concurrently. Sharing across threads therefore
// cannot race, which is what `Send + Sync` require here.
unsafe impl<T: Send> Send for Channel<T> {}
unsafe impl<T: Send> Sync for Channel<T> {}

impl<T> Channel<T> {
    /// `slots` is rounded up to a power of two so index wrapping is a mask.
    pub fn new(slots: usize, budget: usize) -> Self {
        let slots = slots.max(1).next_power_of_two();
        let mut items: Vec<MaybeUninit<(T, usize)>> = Vec::with_capacity(slots);
        for _ in 0..slots {
            items.push(MaybeUninit::uninit());
        }
        Self {
            items: UnsafeCell::new(items),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            mask: slots - 1,
            slots,
            budget,
            charged: AtomicUsize::new(0),
        }
    }

    fn base(&self) -> *mut (T, usize) {
        // Safety: `items` is sized once here and never reallocated, so the pointer
        // stays valid and the length never changes.
        unsafe { (*self.items.get()).as_mut_ptr().cast::<(T, usize)>() }
    }

    /// Producer side. `bytes` is the decoded item's wire size, and travels with
    /// the item so the consumer releases exactly what the producer reserved.
    pub fn push(&self, value: T, bytes: usize) -> Result<(), (T, Rejected)> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= self.slots {
            // Report the item back so a caller can log or drop it deliberately.
            return Err((value, Rejected::Full));
        }
        let charged = self.charged.load(Ordering::Relaxed);
        if charged.saturating_add(bytes) > self.budget {
            return Err((value, Rejected::Full));
        }
        // Safety: `head - tail < slots` proves the consumer released this slot, and
        // the producer is its only writer. The size is stored beside the item so
        // the consumer can release precisely this reservation rather than a
        // running total, which cannot be attributed to the right item.
        unsafe { self.base().add(head & self.mask).write((value, bytes)) };
        self.charged.store(charged + bytes, Ordering::Relaxed);
        // Release: the slot is fully written before a consumer may observe it.
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Consumer side. Takes ownership, releasing the reservation it carried.
    pub fn pop(&self) -> Option<(T, usize)> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        // Safety: `tail != head` proves the producer published this slot and
        // finished writing it. Reading it leaves the slot logically
        // uninitialized, which is the state the next `push` expects.
        let (value, bytes) = unsafe { self.base().add(tail & self.mask).read() };
        // Release exactly this item's reservation, so the budget stays a live
        // window rather than a lifetime total.
        let charged = self.charged.load(Ordering::Relaxed);
        self.charged
            .store(charged.saturating_sub(bytes), Ordering::Relaxed);
        // Release the slot only after ownership has moved out, so the producer
        // cannot overwrite storage that is still being read.
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some((value, bytes))
    }

    pub fn len(&self) -> usize {
        self.head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail.load(Ordering::Acquire))
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
    /// Reports occupancy, not contents. Reading the queue would require taking
    /// the consumer's cursor, which is exactly what the consumer owns.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Channel")
            .field("len", &self.len())
            .field("capacity", &self.slots)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl<T> Drop for Channel<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}
