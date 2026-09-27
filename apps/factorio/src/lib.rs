//! One shared development workspace. All domain transitions run in the caller's
//! Store transaction. Hosts commit intent before Git, setup or OpenCode effects.
#![no_std]
extern crate alloc;

use alloc::{collections::BTreeMap, string::String, vec, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_access::{Access, Audience, KindDefinition};
use snap_document::{Definition, Registry, Snapshot};
use snap_oidc::relying_party as rp;
use snap_store::{Error, Transaction};

pub const WORKSPACE: &str = "faca0000-0000-4000-8000-000000000001";
const OWNER: &str = "factorio-service";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repository: String,
    pub mainline: String,
    /// Crate name to repository-relative directory. `*` claims the entire repo.
    pub modules: BTreeMap<String, String>,
    pub resources: String,
    pub first_port: u16,
    /// Finite, idempotent argv hooks. The host supplies session-scoped environment.
    pub setup: Vec<String>,
    pub teardown: Vec<String>,
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
    pub title: String,
    pub description: String,
    pub modules: Vec<String>,
    pub status: Status,
    pub notes: String,
    pub parent: Option<String>,
    pub blockers: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Active,
    Published,
    Integrating,
    Cleanup,
    Complete,
    Abandoning,
    Abandoned,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub commit: String,
    pub target: String,
    pub evidence: String,
    pub findings: Vec<Finding>,
    pub approval: Option<Approval>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub text: String,
    pub disposition: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Approval {
    pub human: String,
    pub at: i64,
    pub commit: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub owner: String,
    pub prompt: String,
    pub tickets: Vec<String>,
    pub modules: Vec<String>,
    pub phase: Phase,
    pub base: String,
    pub branch: String,
    pub worktree: String,
    pub data: String,
    pub port: u16,
    pub conversation: String,
    pub candidate: Option<Candidate>,
    pub publications: Vec<Candidate>,
    /// Persisted before mainline changes; recovery checks ancestry of this exact OID.
    pub integration: Option<String>,
    pub error: String,
}
impl Session {
    pub fn claims(&self) -> bool {
        !matches!(self.phase, Phase::Complete | Phase::Abandoned)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub config: Config,
    pub tickets: BTreeMap<String, Ticket>,
    pub sessions: BTreeMap<String, Session>,
    pub next_port: u32,
}
pub fn registry() -> Registry {
    Registry::new(vec![Definition {
        kind: "factorio-workspace".into(),
        version: "1".into(),
        validate: |v| serde_json::from_value::<Workspace>(v.clone()).is_ok(),
        mutations: vec![],
    }])
    .expect("workspace schema")
}
pub fn document() -> snap_document::server::Document {
    snap_document::server::Document::new(
        registry(),
        Access::new(vec![KindDefinition::kind("document").unwrap()]).unwrap(),
    )
}
pub fn initialize(tx: &mut Transaction<'_>, config: &Config) -> Result<(), Error> {
    match document().read(tx, WORKSPACE, Some(OWNER)) {
        Ok(snapshot) => {
            let w: Workspace =
                serde_json::from_value(snapshot.value).map_err(|_| Error::Invalid)?;
            if w.config != *config {
                return Err(Error::Constraint);
            }
            Ok(())
        }
        Err(Error::NotFound) => {
            if config.modules.is_empty() || config.first_port < 1024 {
                return Err(Error::Invalid);
            }
            let w = Workspace {
                config: config.clone(),
                tickets: BTreeMap::new(),
                sessions: BTreeMap::new(),
                next_port: config.first_port as u32,
            };
            document().create(
                tx,
                &Snapshot {
                    id: WORKSPACE.into(),
                    kind: "factorio-workspace".into(),
                    version: "1".into(),
                    revision: 1,
                    value: serde_json::to_value(w).map_err(|_| Error::Invalid)?,
                },
                Audience::Authenticated,
                OWNER,
            )?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}
pub fn load(tx: &mut Transaction<'_>) -> Result<Workspace, Error> {
    serde_json::from_value(document().read(tx, WORKSPACE, Some(OWNER))?.value)
        .map_err(|_| Error::Invalid)
}
fn save(tx: &mut Transaction<'_>, w: &Workspace) -> Result<(), Error> {
    document().replace(
        tx,
        WORKSPACE,
        OWNER,
        serde_json::to_value(w).map_err(|_| Error::Invalid)?,
    )?;
    Ok(())
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
fn overlaps(a: &[String], b: &[String]) -> bool {
    a.iter()
        .any(|m| m == "*" || b.iter().any(|n| n == "*" || m == n))
}
pub fn actionable(w: &Workspace, t: &Ticket) -> bool {
    t.status == Status::Ready
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
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn oid(id: &str) -> bool {
    matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Browser and agent callers share the same commands. Only the host's dedicated
/// cookie+CSRF approval route may construct `human = true`; JSON cannot set it.
pub struct Actor<'a> {
    pub session: &'a str,
    pub human: bool,
    pub now: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Ticket {
        ticket: Ticket,
    },
    DeleteTicket {
        id: String,
    },
    Start {
        id: String,
        prompt: String,
        tickets: Vec<String>,
        modules: Vec<String>,
        base: String,
        conversation: String,
    },
    Expand {
        id: String,
        modules: Vec<String>,
    },
    Approve {
        id: String,
        commit: String,
    },
    Abandon {
        id: String,
    },
}
pub fn command(
    tx: &mut Transaction<'_>,
    actor: Actor<'_>,
    cmd: Command,
) -> Result<Workspace, Error> {
    let who = rp::lease(tx, actor.session, actor.now)?;
    let mut w = load(tx)?;
    match cmd {
        Command::Ticket { ticket } => {
            if !valid_id(&ticket.id)
                || ticket.title.trim().is_empty()
                || ticket.title.len() > 200
                || ticket.description.len() + ticket.notes.len() > 32768
            {
                return Err(Error::Invalid);
            }
            scope(&w, &ticket.modules)?;
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
            let id = ticket.id.clone();
            w.tickets.insert(id.clone(), ticket);
            if cycles(&w, &id, false, &mut vec![]) || cycles(&w, &id, true, &mut vec![]) {
                return Err(Error::Constraint);
            }
        }
        Command::DeleteTicket { id } => {
            if w.tickets
                .values()
                .any(|t| t.parent.as_ref() == Some(&id) || t.blockers.contains(&id))
                || w.sessions.values().any(|s| s.tickets.contains(&id))
            {
                return Err(Error::Constraint);
            }
            w.tickets.remove(&id).ok_or(Error::NotFound)?;
        }
        Command::Start {
            id,
            prompt,
            tickets,
            mut modules,
            base,
            conversation,
        } => {
            if !valid_id(&id)
                || !oid(&base)
                || prompt.trim().is_empty()
                || prompt.len() > 32768
                || !conversation.starts_with("ses")
                || !valid_id(&conversation)
            {
                return Err(Error::Invalid);
            }
            if w.sessions.contains_key(&id) {
                return Err(Error::Constraint);
            }
            for id in &tickets {
                let t = w.tickets.get(id).ok_or(Error::NotFound)?;
                if !actionable(&w, t) {
                    return Err(Error::Constraint);
                }
                for m in &t.modules {
                    if !modules.contains(m) {
                        modules.push(m.clone());
                    }
                }
            }
            scope(&w, &modules)?;
            if w.sessions
                .values()
                .any(|s| s.claims() && overlaps(&s.modules, &modules))
            {
                return Err(Error::Constraint);
            }
            let port = u16::try_from(w.next_port).map_err(|_| Error::Constraint)?;
            w.next_port += 1;
            w.sessions.insert(
                id.clone(),
                Session {
                    id: id.clone(),
                    owner: who.owner,
                    prompt,
                    tickets,
                    modules,
                    phase: Phase::Starting,
                    base,
                    branch: alloc::format!("factorio/{id}"),
                    worktree: alloc::format!("{}/worktrees/{id}", w.config.resources),
                    data: alloc::format!("{}/data/{id}", w.config.resources),
                    port,
                    conversation,
                    candidate: None,
                    publications: vec![],
                    integration: None,
                    error: String::new(),
                },
            );
        }
        Command::Expand { id, modules } => {
            scope(&w, &modules)?;
            if w.sessions
                .values()
                .any(|s| s.id != id && s.claims() && overlaps(&s.modules, &modules))
            {
                return Err(Error::Constraint);
            }
            let s = w.sessions.get_mut(&id).ok_or(Error::NotFound)?;
            if !matches!(s.phase, Phase::Active | Phase::Published) {
                return Err(Error::Constraint);
            }
            if let Some(previous) = s.candidate.take() {
                s.publications.push(previous);
            }
            s.phase = Phase::Active;
            for m in modules {
                if !s.modules.contains(&m) {
                    s.modules.push(m);
                }
            }
        }
        Command::Approve { id, commit } => {
            if !actor.human {
                return Err(Error::Constraint);
            }
            let s = w.sessions.get_mut(&id).ok_or(Error::NotFound)?;
            if s.phase != Phase::Published {
                return Err(Error::Constraint);
            }
            let c = s.candidate.as_mut().ok_or(Error::Constraint)?;
            if c.commit != commit || c.findings.iter().any(|f| f.disposition.trim().is_empty()) {
                return Err(Error::Constraint);
            }
            c.approval = Some(Approval {
                human: who.owner,
                at: actor.now,
                commit,
            });
        }
        Command::Abandon { id } => {
            let s = w.sessions.get_mut(&id).ok_or(Error::NotFound)?;
            if !matches!(
                s.phase,
                Phase::Starting | Phase::Active | Phase::Published | Phase::Abandoning
            ) {
                return Err(Error::Constraint);
            }
            s.phase = Phase::Abandoning;
        }
    }
    save(tx, &w)?;
    Ok(w)
}

/// Host-only observations of committed effect intent. Never deserialize this enum
/// from a public request. Claims persist through failures and cleanup retries.
pub enum Effect {
    Started,
    Published {
        commit: String,
        target: String,
        evidence: String,
        findings: Vec<Finding>,
    },
    Integrating {
        commit: String,
    },
    Integrated,
    Cleaned,
    Failed(String),
}
pub fn effect(tx: &mut Transaction<'_>, id: &str, effect: Effect) -> Result<Workspace, Error> {
    if matches!(
        effect,
        Effect::Published { .. } | Effect::Integrating { .. }
    ) {
        return Err(Error::Constraint);
    }
    apply_effect(tx, id, effect)
}

/// Publication and new integration intent require current initiating authority
/// after preparatory IO. Recovery of already committed intent uses `effect`.
pub fn authorized_intent(
    tx: &mut Transaction<'_>,
    actor: Actor<'_>,
    id: &str,
    effect: Effect,
) -> Result<Workspace, Error> {
    if !matches!(
        effect,
        Effect::Published { .. } | Effect::Integrating { .. }
    ) {
        return Err(Error::Invalid);
    }
    rp::lease(tx, actor.session, actor.now)?;
    apply_effect(tx, id, effect)
}

fn apply_effect(tx: &mut Transaction<'_>, id: &str, effect: Effect) -> Result<Workspace, Error> {
    let mut w = load(tx)?;
    if matches!(effect, Effect::Integrating { .. })
        && w.sessions
            .values()
            .any(|s| s.id != id && s.phase == Phase::Integrating)
    {
        return Err(Error::Constraint);
    }
    let s = w.sessions.get_mut(id).ok_or(Error::NotFound)?;
    match effect {
        Effect::Started => {
            if s.phase != Phase::Starting {
                return Err(Error::Constraint);
            }
            s.phase = Phase::Active;
        }
        Effect::Published {
            commit,
            target,
            evidence,
            findings,
        } => {
            if !matches!(s.phase, Phase::Active | Phase::Published)
                || !oid(&commit)
                || !oid(&target)
                || evidence.trim().is_empty()
                || evidence.len() > 32768
            {
                return Err(Error::Constraint);
            }
            if let Some(previous) = s.candidate.take() {
                s.publications.push(previous);
            }
            s.candidate = Some(Candidate {
                commit,
                target,
                evidence,
                findings,
                approval: None,
            });
            s.phase = Phase::Published;
        }
        Effect::Integrating { commit } => {
            if s.phase != Phase::Published
                || !oid(&commit)
                || s.candidate
                    .as_ref()
                    .is_none_or(|c| c.approval.as_ref().is_none_or(|a| a.commit != c.commit))
            {
                return Err(Error::Constraint);
            }
            s.integration = Some(commit);
            s.phase = Phase::Integrating;
        }
        Effect::Integrated => {
            if s.phase != Phase::Integrating || s.integration.is_none() {
                return Err(Error::Constraint);
            }
            // Merge completion and ticket completion cannot be observed separately.
            for id in &s.tickets {
                w.tickets.get_mut(id).ok_or(Error::NotFound)?.status = Status::Done;
            }
            s.phase = Phase::Cleanup;
        }
        Effect::Cleaned => {
            s.phase = match s.phase {
                Phase::Cleanup => Phase::Complete,
                Phase::Abandoning => Phase::Abandoned,
                _ => return Err(Error::Constraint),
            };
        }
        Effect::Failed(message) => {
            s.error = message;
            save(tx, &w)?;
            return Ok(w);
        }
    }
    s.error.clear();
    save(tx, &w)?;
    Ok(w)
}
