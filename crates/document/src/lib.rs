//! Whole-document replication and optimistic intent replay. Hosts own IO and
//! logical connection lifetimes; Store owns durability and transaction exclusion.
#![no_std]
extern crate alloc;
pub mod access;
pub mod operations;
pub mod sync;
pub use access::DocumentAccessGuard;

pub mod client;
mod definition;
mod protocol;
pub mod server;
pub mod wire;

pub use definition::*;
pub use protocol::*;
