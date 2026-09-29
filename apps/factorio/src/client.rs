//! Native/browser operation names and typed application inputs. Carriers choose
//! their IO and correlation; domain types are shared without a JS boundary.
use crate::Command;
use alloc::string::String;
use serde::Serialize;

#[derive(Serialize)]
pub struct WorkspaceInput<'a> {
    pub workspace: &'a str,
}
#[derive(Serialize)]
pub struct CommandInput<'a> {
    pub workspace: &'a str,
    pub command: Command,
}
#[derive(Serialize)]
pub struct IntakeInput<'a> {
    pub workspace: &'a str,
    pub id: &'a str,
}
#[derive(serde::Deserialize)]
pub struct WorkspaceSummary {
    pub id: String,
    pub repository: String,
}
