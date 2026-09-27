//! Host-owned outbound HTTP contract with bounded, incremental bodies.
#![no_std]
extern crate alloc;
pub mod client;
use alloc::boxed::Box;
use core::{future::Future, pin::Pin};
pub type FutureValue<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
