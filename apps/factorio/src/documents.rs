//! Owned workspace graphs. The aggregate Workspace is a domain view, never the
//! stored Document: tickets, sessions and intakes have independent identities.
use super::*;
use alloc::format;
use sha2::{Digest, Sha256};
use snap_access::{
    Access, Actor as AccessActor, Audience, ChangeSet, GrantChange, KindDefinition, LinkChange,
    Resource, Role,
};
use snap_document::{Definition, Registry, Snapshot};
use snap_store::Transaction;

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
        Definition {
            kind: TICKET_KIND.into(),
            version: "1".into(),
            validate: |v| serde_json::from_value::<Child<Ticket>>(v.clone()).is_ok(),
            mutations: vec![snap_document::Mutation {
                name: "ticket.edit".into(),
                minimum: Role::Editor,
                guard: Some(|tx, snapshot, intent, actor, _| {
                    let child: Child<Ticket> = decode(snapshot.value.clone())?;
                    let ticket: Ticket = decode(intent.args.clone())?;
                    if ticket.id != child.data.id {
                        return Ok(false);
                    }
                    let state = load(tx, &child.workspace, actor)?;
                    // Intake edits also advance draft revisions and use the composed
                    // command handler; a single-Document edit cannot skip that write.
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
                    // Match composed commands: editing must not reorder work, and
                    // an undated legacy Document must stay undated.
                    ticket.created_at = child.data.created_at;
                    child.data = ticket;
                    serde_json::to_value(child).map_err(|_| snap_document::Error::Invalid)
                },
            }],
        },
        Definition {
            kind: SESSION_KIND.into(),
            version: "1".into(),
            validate: |v| serde_json::from_value::<Child<Session>>(v.clone()).is_ok(),
            mutations: vec![],
        },
        Definition {
            kind: INTAKE_KIND.into(),
            version: "1".into(),
            validate: |v| serde_json::from_value::<Child<intake::Intake>>(v.clone()).is_ok(),
            mutations: vec![],
        },
    ])
    .expect("Factorio Document definitions")
}

pub fn document() -> snap_document::server::Document {
    snap_document::server::Document::new(
        registry(),
        Access::new(vec![KindDefinition::kind("document").unwrap()]).unwrap(),
    )
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::Invalid)
}
fn encode(value: &impl Serialize) -> Result<serde_json::Value, Error> {
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
    let doc = document();
    if !snap_access::allows(
        doc.access.role(
            tx,
            &Resource::new("document", workspace)?,
            Some(actor),
            false,
        )?,
        Role::Owner,
    ) {
        return Err(Error::NotFound);
    }
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

fn save(
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

pub fn create_intake(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    id: &str,
    description: &str,
) -> Result<intake::Intake, Error> {
    require_owner(tx, workspace, actor)?;
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
    let item = intake::Intake {
        id: id.into(),
        owner: actor.into(),
        description: description.into(),
        conversation: format!(
            "ses_{}",
            next_id(workspace, INTAKE_KIND, id, &root.generations)
        ),
        route: intake::Route::Explore,
        rationale: String::new(),
        tickets: vec![],
        revision: 0,
    };
    let mut after = before.clone();
    after.intakes.insert(id.into(), item.clone());
    save(tx, workspace, actor, &before, &after)?;
    Ok(item)
}

pub fn require_owner(tx: &mut Transaction<'_>, workspace: &str, actor: &str) -> Result<(), Error> {
    if snap_access::allows(
        document().access.role(
            tx,
            &Resource::new("document", workspace)?,
            Some(actor),
            false,
        )?,
        Role::Owner,
    ) {
        Ok(())
    } else {
        Err(Error::NotFound)
    }
}

pub fn ready(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    id: &str,
    revision: u32,
) -> Result<intake::Intake, Error> {
    require_owner(tx, workspace, actor)?;
    let before = load(tx, workspace, actor)?;
    let mut after = before.clone();
    let mut item = before.intakes.get(id).ok_or(Error::NotFound)?.clone();
    if item.revision != revision || item.route != intake::Route::Implement {
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

pub fn delete_intake(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    id: &str,
) -> Result<(), Error> {
    require_owner(tx, workspace, actor)?;
    let before = load(tx, workspace, actor)?;
    let mut after = before.clone();
    after.intakes.remove(id).ok_or(Error::NotFound)?;
    save(tx, workspace, actor, &before, &after)
}

/// Compose every ticket change and the intake revision in one caller transaction.
/// Ordinary workspace Access is the only credential; there is no intake-only key.
pub fn drafts(
    tx: &mut Transaction<'_>,
    workspace: &str,
    actor: &str,
    id: &str,
    input: intake::Drafts,
    now: i64,
) -> Result<intake::Intake, Error> {
    require_owner(tx, workspace, actor)?;
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

fn next_id(workspace: &str, kind: &str, key: &str, generations: &BTreeMap<String, u64>) -> String {
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
            let mut links = ChangeSet::new(AccessActor::identity(actor)?);
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
            document().access.change(tx, &links)?;
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
