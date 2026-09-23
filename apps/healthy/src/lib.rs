//! Application composition only. The executable belongs to the selected host.
#![no_std]

extern crate alloc;

pub mod client;

use alloc::vec;
use snap_protocol::Error;
use snap_runtime::{doctor, transport::Transport};

pub fn application() -> Result<Transport<()>, Error> {
    Transport::new((), vec![doctor::up()])
}
