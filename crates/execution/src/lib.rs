//! IO-free application execution, serialized across a whole application instance.
//!
//! Implement [`Program`] for application admission and private attempts. The host
//! drives [`Executor::step`] and supplies requested inputs. No application stack,
//! future, callback, or mutable reference survives an entry-point return.
#![no_std]
extern crate alloc;

mod executor;
mod program;

pub use executor::{Event, Executor, Scope, Snapshot, Ticket};
pub use program::{
    Admission, Attempt, Call, Error, Inputs, Operation, Outcome, Program, Stop, Validator, View,
    WorkingSet,
};
pub use serde_json::{Value, json};
