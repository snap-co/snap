//! Host-controlled blocking application execution. Transport owns admission and
//! its FIFO; Store owns transactions; selected participants interpret commits.
#![no_std]
extern crate alloc;
mod blocking;
mod participant;
pub use blocking::{Blocking, Peer};
pub use participant::{CommitContext, Connection, InvocationScope, Participant, Progress};
