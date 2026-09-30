//! Conversational intake metadata. OpenCode owns messages; Factorio owns the
//! original request, routing decision and atomically validated ticket drafts.
use super::*;
use alloc::format;
use snap_store::Transaction;
use workspaces::{INTAKE_KIND, load, next_id, root, save};

pub(crate) mod operations;

pub fn definition() -> snap_document::Definition {
    snap_document::Definition {
        kind: workspaces::INTAKE_KIND.into(),
        version: "1".into(),
        validate: |v| serde_json::from_value::<workspaces::Child<Intake>>(v.clone()).is_ok(),
        mutations: vec![],
    }
}

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

/// No authorization is performed in feature persistence. Dispatch captures
/// workspace authority once before entering these composed transitions.
pub fn create(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    id: &str,
    description: &str,
) -> Result<Intake, Error> {
    if !valid_id(id) || id.len() > 48 || description.trim().is_empty() || description.len() > 16384
    {
        return Err(Error::Invalid);
    }
    let before = load(tx, workspace, actor)?;
    if let Some(item) = before.intakes.get(id) {
        return if item.owner == actor && item.description == description {
            Ok(item.clone())
        } else {
            Err(Error::Constraint)
        };
    }
    let root = root(tx, workspace, actor)?;
    let item = Intake {
        id: id.into(),
        owner: actor.into(),
        description: description.into(),
        conversation: format!(
            "ses_{}",
            next_id(workspace, INTAKE_KIND, id, &root.generations)
        ),
        route: Route::Explore,
        rationale: String::new(),
        tickets: vec![],
        revision: 0,
    };
    let mut after = before.clone();
    after.intakes.insert(id.into(), item.clone());
    save(tx, workspace, actor, &before, &after)?;
    Ok(item)
}
pub fn ready(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    id: &str,
    revision: u32,
) -> Result<Intake, Error> {
    let before = load(tx, workspace, actor)?;
    let mut after = before.clone();
    let mut item = before.intakes.get(id).ok_or(Error::NotFound)?.clone();
    if item.revision != revision || item.route != Route::Implement {
        return Err(Error::Constraint);
    }
    let mut changed = false;
    for key in &item.tickets {
        if before
            .tickets
            .values()
            .any(|ticket| ticket.parent.as_ref() == Some(key))
        {
            continue;
        }
        let mut ticket = before.tickets.get(key).ok_or(Error::NotFound)?.clone();
        if ticket.status != Status::Draft {
            continue;
        }
        if ticket.modules.len() != 1 {
            return Err(Error::Constraint);
        }
        ticket.status = Status::Ready;
        after = transition(after, actor, false, 0, Command::Ticket { ticket })?;
        changed = true;
    }
    if !changed {
        return Err(Error::Constraint);
    }
    item.revision = item.revision.checked_add(1).ok_or(Error::Constraint)?;
    after.intakes.insert(id.into(), item.clone());
    save(tx, workspace, actor, &before, &after)?;
    Ok(item)
}
pub fn delete(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    id: &str,
) -> Result<(), Error> {
    let before = load(tx, workspace, actor)?;
    let mut after = before.clone();
    after.intakes.remove(id).ok_or(Error::NotFound)?;
    save(tx, workspace, actor, &before, &after)
}
/// Ticket drafts and the intake revision commit atomically in one transaction.
pub fn drafts(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    id: &str,
    input: Drafts,
    now: i64,
) -> Result<Intake, Error> {
    let before = load(tx, workspace, actor)?;
    let mut after = before.clone();
    let mut item = before.intakes.get(id).ok_or(Error::NotFound)?.clone();
    if item.revision != input.revision || input.tickets.len() > 32 || input.rationale.len() > 8192 {
        return Err(Error::Constraint);
    }
    for ticket in input.tickets {
        if !ticket.id.starts_with(&format!("{id}-"))
            || ticket.status != Status::Draft
            || before.tickets.get(&ticket.id).is_some_and(|old| {
                !item.tickets.contains(&ticket.id) || old.status != Status::Draft
            })
        {
            return Err(Error::Constraint);
        }
        if !item.tickets.contains(&ticket.id) {
            item.tickets.push(ticket.id.clone());
        }
        after = transition(after, actor, false, now, Command::Ticket { ticket })?;
    }
    item.route = input.route;
    item.rationale = input.rationale;
    item.revision = item.revision.checked_add(1).ok_or(Error::Constraint)?;
    after.intakes.insert(id.into(), item.clone());
    save(tx, workspace, actor, &before, &after)?;
    Ok(item)
}
