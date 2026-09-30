//! Owned workspace graphs. The aggregate Workspace is a domain view, never the
//! stored Document: tickets, sessions and intakes have independent identities.
use super::*;
use alloc::format;
use sha2::{Digest, Sha256};
use snap_access::{
    Actor as AccessActor, Audience, ChangeSet, GrantChange, LinkChange, Resource, Role,
};
use snap_document::{Definition, Registry, Snapshot};
use snap_store::Transaction;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repository: String,
    pub mainline: String,
    pub modules: BTreeMap<String, String>,
    pub resources: String,
    pub first_port: u16,
    pub setup: Vec<String>,
    pub teardown: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub config: Config,
    pub tickets: BTreeMap<String, Ticket>,
    pub sessions: BTreeMap<String, Session>,
    pub next_port: u32,
    #[serde(default)]
    pub intakes: BTreeMap<String, intake::Intake>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Recover {
        id: String,
    },
    Publish {
        id: String,
        evidence: String,
        findings: Vec<Finding>,
    },
    Accept {
        id: String,
    },
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

pub const WORKSPACE_KIND: &str = "factorio.workspace";
pub const TICKET_KIND: &str = "factorio.ticket";
pub const SESSION_KIND: &str = "factorio.session";
pub const INTAKE_KIND: &str = "factorio.intake";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub config: Config,
    pub next_port: u32,
    pub tickets: BTreeMap<String, String>,
    pub sessions: BTreeMap<String, String>,
    pub intakes: BTreeMap<String, String>,
    /// Next incarnation per logical key. Retained Documents are never reused, so
    /// old mutation receipts cannot apply to a replacement ticket or intake.
    #[serde(default)]
    pub generations: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Child<T> {
    pub workspace: String,
    pub data: T,
}

pub fn registry() -> Registry {
    Registry::new(vec![
        Definition {
            kind: WORKSPACE_KIND.into(),
            version: "1".into(),
            validate: |v| serde_json::from_value::<Root>(v.clone()).is_ok(),
            mutations: vec![],
        },
        tickets::definition(),
        sessions::definition(),
        intake::definition(),
    ])
    .expect("Factorio Document definitions")
}

pub fn document() -> snap_document::server::Document {
    snap_document::server::Document::new(registry())
}

pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::Invalid)
}
pub(crate) fn encode(value: &impl Serialize) -> Result<serde_json::Value, Error> {
    serde_json::to_value(value).map_err(|_| Error::Invalid)
}

/// Deterministic UUIDs make child creation recoverable without global ticket IDs.
/// Length prefixes avoid ambiguous tuples; workspace identity isolates namespaces.
pub fn child_id(workspace: &str, kind: &str, key: &str) -> String {
    let mut hash = Sha256::new();
    for field in ["factorio-document-v1", workspace, kind, key] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    let mut bytes: [u8; 16] = hash.finalize()[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 15) | 0x50;
    bytes[8] = (bytes[8] & 63) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The host selects an existing repository and validates its paths before calling
/// onboarding. Only the authenticated creator receives the initial owner grant.
pub fn onboard(
    tx: &mut Transaction<'_>,
    id: &str,
    owner: &str,
    mut config: Config,
) -> Result<Snapshot, Error> {
    if config.modules.is_empty() || config.first_port < 1024 {
        return Err(Error::Invalid);
    }
    config.resources = format!("{}/{}", config.resources.trim_end_matches('/'), id);
    let root = Root {
        next_port: config.first_port.into(),
        config,
        tickets: BTreeMap::new(),
        sessions: BTreeMap::new(),
        intakes: BTreeMap::new(),
        generations: BTreeMap::new(),
    };
    let snapshot = Snapshot {
        id: id.into(),
        kind: WORKSPACE_KIND.into(),
        version: "1".into(),
        revision: 1,
        value: encode(&root)?,
    };
    document().create(tx, &snapshot, Audience::Restricted, owner)?;
    Ok(snapshot)
}

pub fn root(tx: &mut Transaction<'_>, id: &str, actor: &str) -> Result<Root, Error> {
    let snapshot = document().read(tx, id, Some(actor))?;
    if snapshot.kind != WORKSPACE_KIND
        || document().lifecycle(tx, id)?.state != snap_document::lifecycle::State::Active
    {
        return Err(Error::NotFound);
    }
    decode(snapshot.value)
}

pub fn load(tx: &mut Transaction<'_>, id: &str, actor: &str) -> Result<Workspace, Error> {
    let root = root(tx, id, actor)?;
    Ok(Workspace {
        config: root.config,
        next_port: root.next_port,
        tickets: children(tx, id, actor, TICKET_KIND, root.tickets)?,
        sessions: children(tx, id, actor, SESSION_KIND, root.sessions)?,
        intakes: children(tx, id, actor, INTAKE_KIND, root.intakes)?,
    })
}

/// Controller-only view. The caller explicitly loads the root and referenced
/// children before entering this read-only transaction; no storage IO is hidden.
pub fn retained(tx: &mut Transaction<'_>, id: &str) -> Result<Workspace, Error> {
    let snapshot = document().retained(tx, id)?;
    if snapshot.kind != WORKSPACE_KIND {
        return Err(Error::Invalid);
    }
    let root: Root = decode(snapshot.value)?;
    Ok(Workspace {
        config: root.config,
        next_port: root.next_port,
        tickets: retained_children(tx, id, TICKET_KIND, root.tickets)?,
        sessions: retained_children(tx, id, SESSION_KIND, root.sessions)?,
        intakes: retained_children(tx, id, INTAKE_KIND, root.intakes)?,
    })
}

fn retained_children<T: serde::de::DeserializeOwned>(
    tx: &mut Transaction<'_>,
    workspace: &str,
    kind: &str,
    index: BTreeMap<String, String>,
) -> Result<BTreeMap<String, T>, Error> {
    let mut values = BTreeMap::new();
    for (key, id) in index {
        let snapshot = document().retained(tx, &id)?;
        let child: Child<T> = decode(snapshot.value)?;
        if snapshot.kind != kind || child.workspace != workspace {
            return Err(Error::Invalid);
        }
        values.insert(key, child.data);
    }
    Ok(values)
}

/// Publish resource observations atomically with ticket completion. Controller
/// ownership does not disappear when a user disconnects or transfers Access.
pub fn observe(
    tx: &mut Transaction<'_>,
    workspace: &str,
    id: &str,
    effect: Effect,
) -> Result<Workspace, Error> {
    let root: Root = decode(document().retained(tx, workspace)?.value)?;
    let before = retained(tx, workspace)?;
    let after = super::observe(before.clone(), id, effect)?;
    for (key, ticket) in &after.tickets {
        if encode(ticket)? != encode(before.tickets.get(key).ok_or(Error::Invalid)?)? {
            document().observe(
                tx,
                root.tickets.get(key).ok_or(Error::NotFound)?,
                encode(&Child {
                    workspace: workspace.into(),
                    data: ticket,
                })?,
            )?;
        }
    }
    let session = after.sessions.get(id).ok_or(Error::NotFound)?;
    let doc_id = root.sessions.get(id).ok_or(Error::NotFound)?;
    document().observe(
        tx,
        doc_id,
        encode(&Child {
            workspace: workspace.into(),
            data: session,
        })?,
    )?;
    if !session.claims() {
        let mut lifecycle = document().lifecycle(tx, doc_id)?;
        lifecycle.finalizers.remove("factorio.session.resources");
        document().set_lifecycle(tx, doc_id, &lifecycle)?;
    }
    Ok(after)
}

fn children<T: serde::de::DeserializeOwned>(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    kind: &str,
    index: BTreeMap<String, String>,
) -> Result<BTreeMap<String, T>, Error> {
    let mut values = BTreeMap::new();
    for (key, id) in index {
        // Retained sessions keep their claims until the cleanup controller marks
        // them complete/abandoned. Hiding a Document must not free repository work.
        if kind != SESSION_KIND
            && document().lifecycle(tx, &id)?.state != snap_document::lifecycle::State::Active
        {
            continue;
        }
        let snapshot = document().read(tx, &id, Some(actor))?;
        if snapshot.kind != kind {
            return Err(Error::Invalid);
        }
        let child: Child<T> = decode(snapshot.value)?;
        if child.workspace != workspace {
            return Err(Error::Invalid);
        }
        values.insert(key, child.data);
    }
    Ok(values)
}

/// Admission and execution both use this domain check in their respective Store
/// transactions. The host captures identity/human proof before ACK.
pub fn guard(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    human: bool,
    now: i64,
    command: Command,
) -> Result<Workspace, Error> {
    let mut next = transition(load(tx, workspace, actor)?, actor, human, now, command)?;
    for session in next.sessions.values_mut() {
        session.branch = format!("factorio/{workspace}/{}", session.id);
    }
    Ok(next)
}

pub fn command(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    human: bool,
    now: i64,
    command: Command,
) -> Result<Workspace, Error> {
    let retry = match &command {
        Command::Recover { id } => Some(id.clone()),
        _ => None,
    };
    let before = load(tx, workspace, actor)?;
    let after = guard(tx, workspace, actor, human, now, command)?;
    save(tx, workspace, actor, &before, &after)?;
    if let Some(id) = retry {
        let id = root(tx, workspace, actor)?
            .sessions
            .get(&id)
            .ok_or(Error::NotFound)?
            .clone();
        let mut lifecycle = document().lifecycle(tx, &id)?;
        lifecycle.blocked = None;
        document().set_lifecycle(tx, &id, &lifecycle)?;
    }
    Ok(after)
}

pub(crate) fn save(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    before: &Workspace,
    after: &Workspace,
) -> Result<(), Error> {
    let mut root = root(tx, workspace, actor)?;
    sync(
        tx,
        workspace,
        actor,
        TICKET_KIND,
        &mut root,
        &before.tickets,
        &after.tickets,
    )?;
    sync(
        tx,
        workspace,
        actor,
        SESSION_KIND,
        &mut root,
        &before.sessions,
        &after.sessions,
    )?;
    sync(
        tx,
        workspace,
        actor,
        INTAKE_KIND,
        &mut root,
        &before.intakes,
        &after.intakes,
    )?;
    root.next_port = after.next_port;
    let value = encode(&root)?;
    if value != document().read(tx, workspace, Some(actor))?.value {
        document().replace(tx, workspace, actor, value)?;
    }
    Ok(())
}

pub fn require_owner(tx: &mut Transaction<'_>, workspace: &str, actor: &str) -> Result<(), Error> {
    snap_document::DocumentAccessGuard::require(tx, workspace, Some(actor), Role::Owner)
}

pub use intake::{create as create_intake, delete as delete_intake, drafts, ready};

pub(crate) fn next_id(
    workspace: &str,
    kind: &str,
    key: &str,
    generations: &BTreeMap<String, u64>,
) -> String {
    let generation = generations
        .get(&format!("{kind}:{key}"))
        .copied()
        .unwrap_or(0);
    if generation == 0 {
        child_id(workspace, kind, key)
    } else {
        child_id(workspace, kind, &format!("{key}@{generation}"))
    }
}

fn sync<T: Serialize>(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    kind: &str,
    root: &mut Root,
    before: &BTreeMap<String, T>,
    after: &BTreeMap<String, T>,
) -> Result<(), Error> {
    let index = match kind {
        TICKET_KIND => &mut root.tickets,
        SESSION_KIND => &mut root.sessions,
        INTAKE_KIND => &mut root.intakes,
        _ => return Err(Error::Invalid),
    };
    let generations = &mut root.generations;
    for key in before.keys().filter(|key| !after.contains_key(*key)) {
        let id = index.remove(key).ok_or(Error::Invalid)?;
        document().remove(tx, &id, actor)?;
        // A root written before incarnation tracking has already used generation 0.
        generations.entry(format!("{kind}:{key}")).or_insert(1);
    }
    for (key, data) in after {
        if before.get(key).map(encode).transpose()? == Some(encode(data)?) {
            continue;
        }
        let value = encode(&Child {
            workspace: workspace.into(),
            data,
        })?;
        if let Some(id) = index.get(key) {
            document().replace(tx, id, actor, value)?;
        } else {
            let id = next_id(workspace, kind, key, generations);
            let generation = generations.entry(format!("{kind}:{key}")).or_insert(0);
            *generation = generation.checked_add(1).ok_or(Error::Constraint)?;
            document().create(
                tx,
                &Snapshot {
                    id: id.clone(),
                    kind: kind.into(),
                    version: "1".into(),
                    revision: 1,
                    value,
                },
                Audience::Restricted,
                actor,
            )?;
            let child = Resource::new("document", &id)?;
            // Dispatch already accepted the workspace composition. Access graph
            // maintenance is part of the same transaction, not a second policy.
            let mut links = ChangeSet::new(AccessActor::system());
            links.links.push(LinkChange {
                child: child.clone(),
                parent: Resource::new("document", workspace)?,
                linked: true,
            });
            // Ownership comes only from the workspace; revoking its grant must
            // revoke every child too, rather than leave creator grants behind.
            links.grants.push(GrantChange {
                resource: child,
                identity: actor.into(),
                role: None,
            });
            snap_document::access::vocabulary().change(tx, &links)?;
            if kind == SESSION_KIND {
                let mut lifecycle = document().lifecycle(tx, &id)?;
                lifecycle
                    .finalizers
                    .insert("factorio.session.resources".into());
                document().set_lifecycle(tx, &id, &lifecycle)?;
            }
            index.insert(key.clone(), id);
        }
    }
    Ok(())
}
