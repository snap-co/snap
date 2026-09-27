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

pub fn create(
    tx: &mut Transaction<'_>,
    actor: Actor<'_>,
    id: &str,
    description: &str,
) -> Result<Intake, Error> {
    let who = rp::lease(tx, actor.session, actor.now)?;
    if !valid_id(id) || id.len() > 48 || description.trim().is_empty() || description.len() > 16384
    {
        return Err(Error::Invalid);
    }
    let mut w = load(tx)?;
    if let Some(old) = w.intakes.get(id) {
        return if old.owner == who.owner && old.description == description {
            Ok(old.clone())
        } else {
            Err(Error::Constraint)
        };
    }
    let item = Intake {
        id: id.into(),
        owner: who.owner,
        description: description.into(),
        conversation: alloc::format!("ses_{id}"),
        route: Route::Explore,
        rationale: String::new(),
        tickets: vec![],
        revision: 0,
    };
    w.intakes.insert(id.into(), item.clone());
    save(tx, &w)?;
    Ok(item)
}

/// A batch is a patch to this intake's drafts. Dependencies must precede users.
/// A stale batch, external ticket ID or any invalid ticket rolls back the whole
/// transaction. The narrow intake capability cannot start work or mark it ready.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Drafts {
    pub revision: u32,
    pub route: Route,
    pub rationale: String,
    pub tickets: Vec<Ticket>,
}
pub fn drafts(
    tx: &mut Transaction<'_>,
    actor: Actor<'_>,
    id: &str,
    input: Drafts,
) -> Result<Intake, Error> {
    let who = rp::lease(tx, actor.session, actor.now)?;
    let w = load(tx)?;
    let mut item = w.intakes.get(id).ok_or(Error::NotFound)?.clone();
    if item.owner != who.owner
        || item.revision != input.revision
        || input.tickets.len() > 32
        || input.rationale.len() > 8192
    {
        return Err(Error::Constraint);
    }
    for ticket in input.tickets {
        if !ticket.id.starts_with(&alloc::format!("{id}-"))
            || ticket.status != Status::Draft
            || w.tickets.get(&ticket.id).is_some_and(|old| {
                !item.tickets.contains(&ticket.id) || old.status != Status::Draft
            })
        {
            return Err(Error::Constraint);
        }
        if !item.tickets.contains(&ticket.id) {
            item.tickets.push(ticket.id.clone());
        }
        command(
            tx,
            Actor {
                session: actor.session,
                human: false,
                now: actor.now,
            },
            Command::Ticket { ticket },
        )?;
    }
    item.route = input.route;
    item.rationale = input.rationale;
    item.revision = item.revision.checked_add(1).ok_or(Error::Constraint)?;
    let mut w = load(tx)?;
    w.intakes.insert(id.into(), item.clone());
    save(tx, &w)?;
    Ok(item)
}

pub fn ready(
    tx: &mut Transaction<'_>,
    actor: Actor<'_>,
    id: &str,
    revision: u32,
) -> Result<Intake, Error> {
    let who = rp::lease(tx, actor.session, actor.now)?;
    let mut w = load(tx)?;
    let mut item = w.intakes.get(id).ok_or(Error::NotFound)?.clone();
    if item.owner != who.owner
        || item.revision != revision
        || item.route != Route::Implement
        || item.tickets.is_empty()
    {
        return Err(Error::Constraint);
    }
    let mut changed = false;
    for key in &item.tickets {
        if w.tickets.values().any(|t| t.parent.as_ref() == Some(key)) {
            continue;
        }
        let mut ticket = w.tickets.get(key).ok_or(Error::NotFound)?.clone();
        if ticket.status != Status::Draft {
            continue;
        }
        if ticket.modules.len() != 1 {
            return Err(Error::Constraint);
        }
        ticket.status = Status::Ready;
        command(
            tx,
            Actor {
                session: actor.session,
                human: actor.human,
                now: actor.now,
            },
            Command::Ticket { ticket },
        )?;
        changed = true;
    }
    if !changed {
        return Err(Error::Constraint);
    }
    item.revision = item.revision.checked_add(1).ok_or(Error::Constraint)?;
    w = load(tx)?;
    w.intakes.insert(id.into(), item.clone());
    save(tx, &w)?;
    Ok(item)
}

/// Removing a conversation leaves its tickets in the shared workspace. Scoped
/// tools require the intake to exist, so outstanding credentials stop working.
pub fn delete(tx: &mut Transaction<'_>, actor: Actor<'_>, id: &str) -> Result<(), Error> {
    let who = rp::lease(tx, actor.session, actor.now)?;
    let mut w = load(tx)?;
    let item = w.intakes.get(id).ok_or(Error::NotFound)?;
    if item.owner != who.owner {
        return Err(Error::NotFound);
    }
    w.intakes.remove(id);
    save(tx, &w)
}
