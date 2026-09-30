//! Physical local adapters for portable Transport execution. Hosts own IO.
#[cfg(feature = "web")]
pub mod development;
pub mod memory;
#[cfg(feature = "native")]
pub mod native;
#[cfg(feature = "web")]
pub mod web;
