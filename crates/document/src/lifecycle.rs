//! Persisted cleanup state. Deletion and archival retain the Document while
//! removing it from normal synchronization. Physical purging is not implemented.
use alloc::{collections::BTreeSet, string::String};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Active,
    Deleted,
    Archived,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lifecycle {
    pub state: State,
    pub finalizers: BTreeSet<String>,
    pub blocked: Option<String>,
}
