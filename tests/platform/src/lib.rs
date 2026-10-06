//! Snap-owned platform contracts and controlled dependency support.
//! Cases use production interfaces; hosts supply IO, deadlines and teardown.
//! Transport and Store contracts are independent: carriers do not execute
//! operations, and passing both does not prove a host's dispatch composition.
#![no_std]
extern crate alloc;

pub mod cartridge;
pub mod dispatch;
pub mod journey;
pub mod memory;
pub mod simulation;
pub mod store;
pub mod transport;
