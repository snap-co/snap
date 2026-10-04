//! Host-controlled blocking application execution. Transport owns admission and
//! its FIFO; Store owns transactions; selected participants interpret commits.
#![no_std]
extern crate alloc;
mod blocking;
pub mod controller;
mod participant;
mod subscriptions;
pub use blocking::{Blocking, Peer};
pub use controller::{Controller, ControllerContext, Controllers};
pub use participant::{CommitContext, Connection, InvocationScope, Participant, Progress};
pub use subscriptions::Subscriptions;
pub type Application<B> = Controllers<B, Subscriptions>;
