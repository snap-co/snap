//! Portable Factorio application. Feature declarations and transitions share
//! caller-owned Store transactions; platform bootstrap supplies physical inputs.
#![no_std]
extern crate alloc;
use alloc::{collections::BTreeMap, string::String, vec, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_store::Error;

pub mod client;
pub mod intake;
pub mod login;
pub mod operations;
pub mod sessions;
pub mod tickets;
pub mod workspaces;

/// Initial migration for Factorio-owned tables in fresh databases. Hosts combine
/// it with their selected module and host-storage migrations.
pub const MIGRATION: &str = include_str!("../migrations/0001_factorio.toml");

/// Explicit portable declarations. Native or Wasm bootstrap chooses providers
/// for the declared clock/entropy inputs and Store backend, never feature policy.
pub struct Application {
    pub requests: Vec<snap_transport::operation::Definition>,
    pub preconnection: Vec<snap_transport::operation::Definition>,
}
pub fn application(config: Config, origin: String) -> Application {
    operations::declarations(config, origin)
}

pub use sessions::{
    Approval, Candidate, Desired, Effect, Finding, Phase, Session, observe, transition,
};
pub use tickets::{Status, Ticket, actionable};
pub use workspaces::{Command, Config, Workspace};

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn scope(w: &Workspace, modules: &[String]) -> Result<(), Error> {
    if modules.is_empty()
        || modules
            .iter()
            .any(|m| m != "*" && !w.config.modules.contains_key(m))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
