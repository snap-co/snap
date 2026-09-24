//! Standalone portability probe, built directly with rustc by run.py.
#![no_std]
#![forbid(unsafe_code)]
#[path = "core.rs"]
pub mod prototype;
