//! Conversational intake metadata. OpenCode owns messages; Factorio owns the
//! original request, routing decision and atomically validated ticket drafts.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Explore,
    Grill,
    Triage,
    Wayfinder,
    Implement,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Intake {
    pub id: String,
    pub owner: String,
    pub description: String,
    pub conversation: String,
    pub route: Route,
    pub rationale: String,
    pub tickets: Vec<String>,
    pub revision: u32,
}

/// A batch is a patch to this intake's drafts. Dependencies must precede users.
/// A stale batch, external ticket ID or any invalid ticket rolls back the whole
/// transaction; readiness is a separate command.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Drafts {
    pub revision: u32,
    pub route: Route,
    pub rationale: String,
    pub tickets: Vec<Ticket>,
}
