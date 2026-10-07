//! Single-threaded testing platform. The scheduler owns time, structured message
//! delivery and production host steps. The Store driver owns dependency faults,
//! never transaction execution or expected application results.
mod store;
mod transport;

pub use store::{CommitFault, Store};
pub use transport::{
    CarrierPolicy, Channel, Clock, Connector, Failure, LossBoundary, NetworkFaults, Simulation,
    Sleep,
};

use crate::runner::Fingerprint;
use alloc::{collections::VecDeque, rc::Rc, string::String, vec::Vec};
use core::cell::RefCell;
use serde::Serialize;

/// All durations are virtual milliseconds. Jitter is drawn independently for
/// each dependency boundary from this schedule seed, not a workload seed.
#[derive(Clone, Copy, Debug)]
pub struct Schedule {
    pub seed: u64,
    pub command_ms: u64,
    /// Time between decoded carrier receipt and the worker's host submission.
    pub admission_ms: u64,
    pub response_ms: u64,
    pub execution_ms: u64,
    pub load_ms: u64,
    pub commit_ms: u64,
    pub jitter_ms: u64,
    pub max_events: usize,
    pub max_polls: usize,
    pub max_time_ms: u64,
    /// Recent event history only. Fingerprinting continues after records expire.
    pub trace_capacity: usize,
}
impl Default for Schedule {
    fn default() -> Self {
        Self {
            seed: 1,
            command_ms: 2,
            admission_ms: 1,
            response_ms: 2,
            execution_ms: 10,
            load_ms: 1,
            commit_ms: 3,
            jitter_ms: 0,
            max_events: 10_000,
            max_polls: 10_000,
            max_time_ms: 1_000_000,
            trace_capacity: 10_000,
        }
    }
}

/// Diagnostics omit credentials and application payloads. They record when the
/// driver observes host output and when it delivers, not invented outcomes.
/// Output observation is at a scheduler boundary, not inside a host callback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Record {
    pub at_ms: u64,
    pub action: Action,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Action {
    PeerOpened {
        peer: u64,
        actor: Option<usize>,
    },
    LossArmed {
        peer: u64,
        id: u64,
        boundary: LossBoundary,
    },
    NetworkLoss {
        peer: u64,
        actor: Option<usize>,
        id: u64,
        boundary: LossBoundary,
    },
    TimerScheduled {
        id: u64,
        deadline_ms: u64,
    },
    TimerFired {
        id: u64,
    },
    TimerCanceled {
        id: u64,
    },
    TaskFired {
        id: u64,
        label: &'static str,
    },
    CommandQueued {
        peer: u64,
        kind: &'static str,
        id: Option<u64>,
    },
    CommandDelivered {
        peer: u64,
        kind: &'static str,
        id: Option<u64>,
    },
    CommandSubmitted {
        peer: u64,
        kind: &'static str,
        id: Option<u64>,
    },
    ResponsePublished {
        peer: u64,
        kind: &'static str,
        id: Option<u64>,
    },
    ResponseDelivered {
        peer: u64,
        kind: &'static str,
        id: Option<u64>,
    },
    Executed {
        progressed: bool,
    },
    Disconnected {
        peer: u64,
        close: bool,
    },
    StoreLoad {
        table: String,
    },
    StoreCommit {
        rejected: bool,
    },
}

/// Shared only by host-owned drivers. Client/server application state remains
/// separate; messages cross the link by owned value. No wall clock or IO is used.
#[derive(Clone)]
pub struct Timeline(Rc<RefCell<Time>>);
struct Time {
    schedule: Schedule,
    now: u64,
    random: u64,
    trace: VecDeque<Record>,
    discarded: u64,
    fingerprint: Fingerprint,
}
impl Timeline {
    pub fn new(schedule: Schedule) -> Self {
        Self(Rc::new(RefCell::new(Time {
            schedule,
            now: 0,
            random: schedule.seed,
            trace: VecDeque::new(),
            discarded: 0,
            fingerprint: Fingerprint::new(b"snap-simulation-events-v1"),
        })))
    }
    pub fn now(&self) -> u64 {
        self.0.borrow().now
    }
    pub fn trace(&self) -> Vec<Record> {
        self.0.borrow().trace.iter().cloned().collect()
    }
    pub fn discarded_records(&self) -> u64 {
        self.0.borrow().discarded
    }
    /// Includes event order and virtual observation times, even for discarded
    /// trace records. Not a hash of internal state or unobserved backend programs.
    pub fn events_sha256(&self) -> [u8; 32] {
        self.0.borrow().fingerprint.finish()
    }
    pub fn schedule(&self) -> Schedule {
        self.0.borrow().schedule
    }
    fn delay(&self, base: u64) -> u64 {
        let mut time = self.0.borrow_mut();
        // SplitMix64: specified integer operations keep replay platform-independent.
        time.random = time.random.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = time.random;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^= value >> 31;
        let jitter = if time.schedule.jitter_ms == u64::MAX {
            value
        } else {
            value % (time.schedule.jitter_ms + 1)
        };
        base.saturating_add(jitter)
    }
    fn advance_to(&self, at: u64) {
        let mut time = self.0.borrow_mut();
        time.now = time.now.max(at);
    }
    fn elapse(&self, base: u64) {
        self.advance_to(self.now().saturating_add(self.delay(base)));
    }
    fn record(&self, action: Action) {
        let mut time = self.0.borrow_mut();
        let at_ms = time.now;
        let record = Record { at_ms, action };
        time.fingerprint.record(&record);
        if time.schedule.trace_capacity == 0 {
            time.discarded += 1;
        } else {
            if time.trace.len() == time.schedule.trace_capacity {
                time.trace.pop_front();
                time.discarded += 1;
            }
            time.trace.push_back(record);
        }
    }
}
