//! Ticket graph transitions and immutable creation dates.
use crate::*;
use workspaces::{Child, decode};

pub fn definition() -> snap_document::Definition {
    snap_document::Definition {
        kind: workspaces::TICKET_KIND.into(),
        version: "1".into(),
        validate: |v| serde_json::from_value::<Child<Ticket>>(v.clone()).is_ok(),
        mutations: vec![snap_document::Mutation {
            name: "ticket.edit".into(),
            minimum: snap_access::Role::Editor,
            guard: Some(|tx, snapshot, intent, actor, _| {
                let child: Child<Ticket> = decode(snapshot.value.clone())?;
                let ticket: Ticket = decode(intent.args.clone())?;
                if ticket.id != child.data.id {
                    return Ok(false);
                }
                let state = workspaces::load(tx, &child.workspace, actor)?;
                // A single-document edit cannot bypass the intake revision write.
                if state
                    .intakes
                    .values()
                    .any(|item| item.tickets.contains(&ticket.id))
                {
                    return Ok(false);
                }
                Ok(transition(state, actor, false, 0, Command::Ticket { ticket }).is_ok())
            }),
            apply: |before, args, _| {
                let mut child: Child<Ticket> = serde_json::from_value(before.clone())
                    .map_err(|_| snap_document::Error::Invalid)?;
                let mut ticket: Ticket = serde_json::from_value(args.clone())
                    .map_err(|_| snap_document::Error::Invalid)?;
                ticket.created_at = child.data.created_at;
                child.data = ticket;
                serde_json::to_value(child).map_err(|_| snap_document::Error::Invalid)
            },
        }],
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Draft,
    Ready,
    Done,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    pub id: String,
    #[serde(default)]
    pub created_at: Option<i64>,
    pub title: String,
    pub description: String,
    pub modules: Vec<String>,
    pub status: Status,
    pub notes: String,
    pub parent: Option<String>,
    pub blockers: Vec<String>,
}

pub fn actionable(w: &Workspace, t: &Ticket) -> bool {
    t.status == Status::Ready
        && t.modules.len() == 1
        && !w
            .tickets
            .values()
            .any(|child| child.parent.as_ref() == Some(&t.id))
        && t.blockers
            .iter()
            .all(|id| w.tickets.get(id).is_some_and(|b| b.status == Status::Done))
        && !w
            .sessions
            .values()
            .any(|s| s.claims() && s.tickets.contains(&t.id))
}
fn cycles(w: &Workspace, id: &str, parents: bool, stack: &mut Vec<String>) -> bool {
    if stack.iter().any(|v| v == id) {
        return true;
    }
    let Some(t) = w.tickets.get(id) else {
        return true;
    };
    stack.push(id.into());
    let result = if parents {
        t.parent.as_ref().is_some_and(|p| cycles(w, p, true, stack))
    } else {
        t.blockers.iter().any(|p| cycles(w, p, false, stack))
    };
    stack.pop();
    result
}
pub(crate) fn edit(w: &mut Workspace, mut ticket: Ticket, now: i64) -> Result<(), Error> {
    if !valid_id(&ticket.id)
        || ticket.title.trim().is_empty()
        || ticket.title.len() > 200
        || ticket.description.len() + ticket.notes.len() > 32768
    {
        return Err(Error::Invalid);
    }
    scope(w, &ticket.modules)?;
    if ticket.status == Status::Ready && ticket.modules.len() != 1 {
        return Err(Error::Constraint);
    }
    let old = w.tickets.get(&ticket.id);
    if ticket.status == Status::Done && old.is_none_or(|t| t.status != Status::Done) {
        return Err(Error::Constraint);
    }
    if old.is_some_and(|t| t.status == Status::Done) && ticket.status != Status::Done {
        return Err(Error::Constraint);
    }
    if w.sessions
        .values()
        .any(|s| s.claims() && s.tickets.contains(&ticket.id))
        && old.is_none_or(|t| {
            t.modules != ticket.modules
                || t.status != ticket.status
                || t.parent != ticket.parent
                || t.blockers != ticket.blockers
        })
    {
        return Err(Error::Constraint);
    }
    ticket.created_at = old.map_or(Some(now), |old| old.created_at);
    let id = ticket.id.clone();
    w.tickets.insert(id.clone(), ticket);
    if cycles(w, &id, false, &mut vec![]) || cycles(w, &id, true, &mut vec![]) {
        return Err(Error::Constraint);
    }
    for item in w
        .intakes
        .values_mut()
        .filter(|item| item.tickets.contains(&id))
    {
        item.revision = item.revision.checked_add(1).ok_or(Error::Constraint)?;
    }
    Ok(())
}
pub(crate) fn delete(w: &mut Workspace, id: &str) -> Result<(), Error> {
    if w.tickets
        .values()
        .any(|t| t.parent.as_deref() == Some(id) || t.blockers.iter().any(|b| b == id))
        || w.sessions
            .values()
            .any(|s| s.tickets.iter().any(|t| t == id))
    {
        return Err(Error::Constraint);
    }
    w.tickets.remove(id).ok_or(Error::NotFound)?;
    for item in w
        .intakes
        .values_mut()
        .filter(|item| item.tickets.iter().any(|t| t == id))
    {
        item.tickets.retain(|ticket| ticket != id);
        item.revision = item.revision.checked_add(1).ok_or(Error::Constraint)?;
    }
    Ok(())
}
