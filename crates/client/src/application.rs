//! Resident client application seam for the current single-flight Query slice.
//! The host executes the next external step, then supplies an input synchronously.
use alloc::string::String;
use snap_protocol::{Error, Invocation};

pub enum Input {
    Start,
    Wake,
    Completed {
        result: Result<String, Error>,
        at: u64,
    },
}

pub enum Step {
    Query(Invocation),
    Wait { milliseconds: u32 },
    Stop,
}

pub trait Application {
    type Snapshot: Clone;

    fn update(&mut self, input: Input) -> Step;
    fn snapshot(&self) -> Self::Snapshot;
}
