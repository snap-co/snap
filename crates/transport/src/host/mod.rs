//! Application composition for blocking execution. Transport holds admission
//! through commit, selected participants and terminal publication. Store owns
//! durability; applications explicitly select controllers and subscriptions.
mod blocking;
pub mod controller;
mod participant;
mod subscriptions;
pub use blocking::{Blocking, Peer};
pub use controller::{Controller, ControllerContext, Controllers};
pub use participant::{CommitContext, Connection, InvocationScope, Participant, Progress};
pub use subscriptions::Subscriptions;
pub type Application<B> = Controllers<B, Subscriptions>;
