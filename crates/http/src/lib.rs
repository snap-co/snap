//! Host-owned outbound HTTP contract with bounded, incremental bodies.
#![no_std]
extern crate alloc;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
extern crate std;
pub mod client;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
pub mod native;
use alloc::boxed::Box;
use core::{future::Future, pin::Pin};
pub type FutureValue<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
