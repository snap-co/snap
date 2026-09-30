//! IO-free operation execution owned by portable Transport.
//!
//! Implement [`Program`] for application admission and private attempts. The host
//! drives [`Executor::step`] and supplies requested inputs. No application stack,
//! future, callback, or mutable reference survives an entry-point return.
mod executor;
mod program;

pub use crate::{Value, json};
pub use executor::{
    Event, Executor, Inspection, JobView, PreparedRequest, Scope, Snapshot, Ticket,
};
pub use program::{
    Admission, Attempt, Call, Error, Inputs, Operation, Outcome, Program, Stop, Validator, View,
    WorkingSet,
};
