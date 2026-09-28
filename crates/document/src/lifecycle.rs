//! Persisted cleanup state. Deletion and archival retain the Document while
//! removing it from normal synchronization. Physical purging is not implemented.
use alloc::{collections::BTreeSet, string::String};
use serde::{Deserialize, Serialize};

/// Server-owned operations. Clients queue these without projecting cleanup state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Delete,
    Archive,
    Retry,
}

impl Operation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Delete => "document.delete",
            Self::Archive => "document.archive",
            Self::Retry => "document.retry",
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        match name {
            "document.delete" => Some(Self::Delete),
            "document.archive" => Some(Self::Archive),
            "document.retry" => Some(Self::Retry),
            _ => None,
        }
    }
}

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
