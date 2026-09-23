//! Host-driven Snap execution. The host supplies inputs and consumes actions.
#![no_std]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use snap_protocol::{Invocation, Operation, Outcome};

pub mod doctor;
pub mod transport;

/// An opaque host delivery address, separate from the caller's operation id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Delivery(pub u64);

#[derive(Debug)]
pub enum Input {
    Invocation {
        delivery: Delivery,
        invocation: Invocation,
    },
}

#[derive(Debug)]
pub enum Action {
    Complete {
        delivery: Delivery,
        operation_id: String,
        outcome: Outcome,
    },
}

/// An in-process Rust seam, not a dynamic-library ABI.
///
/// Each update is synchronous CPU/memory work. The host owns scheduling and IO.
/// The action buffer belongs to the host and is drained after each turn.
pub trait Module {
    fn operations(&self) -> impl Iterator<Item = Operation>;
    fn update(&mut self, input: Input, actions: &mut Vec<Action>);
}
