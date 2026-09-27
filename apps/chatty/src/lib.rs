//! Private conversation transactions. Hosts own OAuth, model/tool IO and task
//! lifetime. Acceptance commits before effects, and every progress write rechecks
//! session, ownership and the active-turn fence in the same Store transaction.
#![no_std]
extern crate alloc;
use alloc::{string::String, vec, vec::Vec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use snap_access::{Access, Audience, KindDefinition, Role};
use snap_document::{Definition, Mutation, Registry, Snapshot};
use snap_oidc::relying_party as rp;
use snap_store::{Error, Row, Transaction, Value as Cell};

pub const MIGRATION: &str = include_str!("../migrations/0001_chatty.toml");
pub const TABLES: [&str; 2] = ["chatty.threads", "chatty.turns"];
pub const KIND: &str = "chatty-thread";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    pub title: String,
    pub effort: String,
    pub created: i64,
    pub updated: i64,
    pub active_turn: String,
    pub turns: Vec<Turn>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    pub id: String,
    pub request_id: String,
    pub user: String,
    pub text: String,
    pub summary: String,
    pub tools: Vec<Value>,
    pub usage: Value,
    pub status: String,
    pub error: String,
    pub created: i64,
}
/// Host-owned effect request, never a browser response. Context contains opaque
/// provider items. Persist only the explicit display projection in Documents.
pub struct Job {
    pub session: String,
    pub owner: String,
    pub thread: String,
    pub turn: String,
    pub effort: String,
    pub input: Vec<Value>,
    pub omitted: usize,
}
pub struct Accepted {
    pub turn: String,
    pub job: Option<Job>,
}
#[derive(Clone, Default)]
pub struct Progress {
    pub text: String,
    pub summary: String,
    pub tools: Vec<Value>,
    pub usage: Value,
    pub output: Vec<Value>,
}
pub enum Outcome<'a> {
    Progress,
    Complete,
    Failed(&'a str),
}

fn valid_effort(value: &str) -> bool {
    matches!(value, "minimal" | "low" | "medium" | "high" | "xhigh")
}
fn valid_title(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 100
}
fn validate(value: &Value) -> bool {
    let Ok(c) = serde_json::from_value::<Conversation>(value.clone()) else {
        return false;
    };
    valid_title(&c.title)
        && valid_effort(&c.effort)
        && c.turns.len() <= 200
        && c.created >= 0
        && c.updated >= 0
        && c.turns.iter().all(|t| {
            !t.user.is_empty()
                && t.user.len() <= 32 * 1024
                && t.text.len() + t.summary.len() <= 512 * 1024
                && matches!(
                    t.status.as_str(),
                    "running" | "complete" | "failed" | "cancelled" | "interrupted"
                )
        })
}
pub fn registry() -> Registry {
    Registry::new(vec![Definition {
        kind: KIND.into(),
        version: "1".into(),
        validate,
        mutations: vec![Mutation {
            name: "rename".into(),
            minimum: Role::Owner,
            guard: None,
            apply: |value, args, _| {
                let title = args["title"]
                    .as_str()
                    .ok_or(snap_document::Error::Invalid)?
                    .trim();
                let effort = args["effort"]
                    .as_str()
                    .ok_or(snap_document::Error::Invalid)?;
                if !valid_title(title) || !valid_effort(effort) {
                    return Err(snap_document::Error::Invalid);
                }
                let mut result = value.clone();
                result["title"] = title.into();
                result["effort"] = effort.into();
                Ok(result)
            },
        }],
    }])
    .expect("thread definition")
}
pub fn document() -> snap_document::server::Document {
    snap_document::server::Document::new(
        registry(),
        Access::new(vec![KindDefinition::kind("document").unwrap()]).unwrap(),
    )
}
fn text(row: &Row, key: &str) -> Result<String, Error> {
    match row.get(key) {
        Some(Cell::Text(s)) => Ok(s.clone()),
        _ => Err(Error::Invalid),
    }
}
fn row(fields: Vec<(&str, Cell)>) -> Row {
    fields.into_iter().map(|(k, v)| (k.into(), v)).collect()
}
fn actor(tx: &mut Transaction<'_>, session: &str, now: i64) -> Result<rp::Session, Error> {
    rp::lease(tx, session, now)
}
fn conversation(tx: &mut Transaction<'_>, owner: &str, id: &str) -> Result<Conversation, Error> {
    let metadata = tx.get(TABLES[0], &[id.into()])?.ok_or(Error::NotFound)?;
    if text(&metadata, "owner")? != owner {
        return Err(Error::NotFound);
    }
    serde_json::from_value(document().read(tx, id, Some(owner))?.value).map_err(|_| Error::Invalid)
}
fn save(tx: &mut Transaction<'_>, owner: &str, id: &str, c: &Conversation) -> Result<(), Error> {
    document().replace(
        tx,
        id,
        owner,
        serde_json::to_value(c).map_err(|_| Error::Invalid)?,
    )?;
    Ok(())
}

pub fn create(
    tx: &mut Transaction<'_>,
    session: &str,
    id: &str,
    title: &str,
    effort: &str,
    now: i64,
) -> Result<(), Error> {
    let actor = actor(tx, session, now)?;
    if now < 0 || !valid_title(title) || !valid_effort(effort) {
        return Err(Error::Invalid);
    }
    if tx
        .find(TABLES[0], "owner", &[actor.owner.clone().into()])?
        .len()
        >= 200
    {
        return Err(Error::Constraint);
    }
    let c = Conversation {
        title: title.trim().into(),
        effort: effort.into(),
        created: now,
        updated: now,
        active_turn: String::new(),
        turns: vec![],
    };
    document().create(
        tx,
        &Snapshot {
            id: id.into(),
            kind: KIND.into(),
            version: "1".into(),
            revision: 1,
            value: serde_json::to_value(c).map_err(|_| Error::Invalid)?,
        },
        Audience::Restricted,
        &actor.owner,
    )?;
    tx.insert(
        TABLES[0],
        row(vec![("id", id.into()), ("owner", actor.owner.into())]),
    )
}
pub fn remove(tx: &mut Transaction<'_>, session: &str, id: &str, now: i64) -> Result<(), Error> {
    let actor = actor(tx, session, now)?;
    conversation(tx, &actor.owner, id)?;
    for turn in tx.find(TABLES[1], "thread", &[id.into()])? {
        tx.delete(TABLES[1], &[text(&turn, "id")?.into()])?;
    }
    document().remove(tx, id, &actor.owner)?;
    tx.delete(TABLES[0], &[id.into()])?;
    Ok(())
}

pub struct Send<'a> {
    pub session: &'a str,
    pub thread: &'a str,
    pub turn: &'a str,
    pub request: &'a str,
    pub message: &'a str,
    pub now: i64,
}
/// Deduplication and acceptance share one transaction. Only `job: Some` owns a new
/// effect; a duplicate with the same message returns its existing turn without IO.
pub fn send(tx: &mut Transaction<'_>, request: Send<'_>) -> Result<Accepted, Error> {
    let Send {
        session,
        thread,
        turn,
        request,
        message,
        now,
    } = request;
    let actor = actor(tx, session, now)?;
    let mut c = conversation(tx, &actor.owner, thread)?;
    let message = message.trim();
    if message.is_empty()
        || message.len() > 32 * 1024
        || request.is_empty()
        || request.len() > 100
        || !request
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(Error::Invalid);
    }
    if let Some(existing) = tx
        .find(TABLES[1], "request", &[thread.into(), request.into()])?
        .first()
    {
        let id = text(existing, "id")?;
        if c.turns
            .iter()
            .find(|t| t.id == id)
            .is_none_or(|t| t.user != message)
        {
            return Err(Error::Constraint);
        }
        return Ok(Accepted {
            turn: id,
            job: None,
        });
    }
    if !c.active_turn.is_empty() || c.turns.len() >= 200 {
        return Err(Error::Constraint);
    }
    let mut active = 0;
    for row in tx.find(TABLES[0], "primary", &[])? {
        if !conversation(tx, &text(&row, "owner")?, &text(&row, "id")?)?
            .active_turn
            .is_empty()
        {
            active += 1;
        }
    }
    if active >= 4 {
        return Err(Error::Constraint);
    }
    let (input, omitted) = context(tx, &c, message)?;
    tx.insert(
        TABLES[1],
        row(vec![
            ("id", turn.into()),
            ("thread", thread.into()),
            ("request", request.into()),
            ("session", session.into()),
            ("output", "[]".into()),
        ]),
    )?;
    c.turns.push(Turn {
        id: turn.into(),
        request_id: request.into(),
        user: message.into(),
        text: String::new(),
        summary: String::new(),
        tools: vec![],
        usage: json!({"context_omitted":omitted}),
        status: "running".into(),
        error: String::new(),
        created: now,
    });
    c.active_turn = turn.into();
    c.updated = now;
    if c.title == "New thread" {
        c.title = message.chars().take(60).collect();
    }
    save(tx, &actor.owner, thread, &c)?;
    Ok(Accepted {
        turn: turn.into(),
        job: Some(Job {
            session: session.into(),
            owner: actor.owner,
            thread: thread.into(),
            turn: turn.into(),
            effort: c.effort,
            input,
            omitted,
        }),
    })
}

fn fenced(tx: &mut Transaction<'_>, job: &Job) -> Result<Conversation, Error> {
    let c = conversation(tx, &job.owner, &job.thread)?;
    let record = tx
        .get(TABLES[1], &[job.turn.clone().into()])?
        .ok_or(Error::NotFound)?;
    if c.active_turn != job.turn
        || text(&record, "session")? != job.session
        || text(&record, "thread")? != job.thread
        || c.turns
            .iter()
            .find(|t| t.id == job.turn)
            .is_none_or(|t| t.status != "running")
    {
        return Err(Error::NotFound);
    }
    Ok(c)
}
pub fn can_continue(tx: &mut Transaction<'_>, job: &Job, now: i64) -> Result<(), Error> {
    if actor(tx, &job.session, now)?.owner != job.owner {
        return Err(Error::NotFound);
    }
    fenced(tx, job)?;
    Ok(())
}
/// Every progress/terminal publication checks current authority and the immutable
/// accepted turn. Cancellation, deletion and logout reject late provider results.
pub fn progress(
    tx: &mut Transaction<'_>,
    job: &Job,
    p: &Progress,
    outcome: Outcome<'_>,
    now: i64,
) -> Result<(), Error> {
    can_continue(tx, job, now)?;
    let mut c = fenced(tx, job)?;
    if p.text.len() + p.summary.len() > 512 * 1024 || p.tools.len() > 8 {
        return Err(Error::Invalid);
    }
    let turn = c
        .turns
        .iter_mut()
        .find(|t| t.id == job.turn)
        .ok_or(Error::NotFound)?;
    turn.text = p.text.clone();
    turn.summary = p.summary.clone();
    turn.tools = p.tools.clone();
    turn.usage = p.usage.clone();
    match outcome {
        Outcome::Progress => {}
        Outcome::Complete => {
            turn.status = "complete".into();
            c.active_turn.clear();
        }
        Outcome::Failed(error) => {
            turn.status = "failed".into();
            turn.error = error.chars().take(1000).collect();
            c.active_turn.clear();
        }
    }
    c.updated = now;
    tx.update(
        TABLES[1],
        &[job.turn.clone().into()],
        row(vec![(
            "output",
            serde_json::to_string(&p.output)
                .map_err(|_| Error::Invalid)?
                .into(),
        )]),
    )?;
    save(tx, &job.owner, &job.thread, &c)
}
pub fn cancel(
    tx: &mut Transaction<'_>,
    session: &str,
    thread: &str,
    turn: &str,
    now: i64,
) -> Result<(), Error> {
    let actor = actor(tx, session, now)?;
    let mut c = conversation(tx, &actor.owner, thread)?;
    if c.active_turn != turn {
        return Err(Error::NotFound);
    }
    let turn = c
        .turns
        .iter_mut()
        .find(|t| t.id == turn)
        .ok_or(Error::NotFound)?;
    turn.status = "cancelled".into();
    turn.error = "Stopped by you. In-flight remote work may still finish.".into();
    c.active_turn.clear();
    c.updated = now;
    save(tx, &actor.owner, thread, &c)
}
/// A host may release only its own fenced work after authority loss. This records
/// interruption without publishing late provider output or granting user authority.
pub fn abandon(tx: &mut Transaction<'_>, job: &Job, now: i64) -> Result<(), Error> {
    let mut c = fenced(tx, job)?;
    let turn = c
        .turns
        .iter_mut()
        .find(|t| t.id == job.turn)
        .ok_or(Error::NotFound)?;
    turn.status = "interrupted".into();
    turn.error = "The session ended during this reply.".into();
    c.active_turn.clear();
    c.updated = now;
    save(tx, &job.owner, &job.thread, &c)
}
/// Startup never repeats an uncertain model request or file write.
pub fn recover(tx: &mut Transaction<'_>, now: i64) -> Result<(), Error> {
    for row in tx.find(TABLES[0], "primary", &[])? {
        let owner = text(&row, "owner")?;
        let id = text(&row, "id")?;
        let mut c = conversation(tx, &owner, &id)?;
        if c.active_turn.is_empty() {
            continue;
        }
        for turn in c.turns.iter_mut().filter(|t| t.status == "running") {
            turn.status = "interrupted".into();
            turn.error =
                "The host restarted during this reply. Send a new message to continue.".into();
        }
        c.active_turn.clear();
        c.updated = now;
        save(tx, &owner, &id, &c)?;
    }
    Ok(())
}
fn replay(items: Vec<Value>) -> Vec<Value> {
    items
        .into_iter()
        .map(|mut item| {
            if item["type"] == "reasoning" && item.get("summary").is_none() {
                item["summary"] = json!([]);
            }
            item
        })
        .collect()
}
fn context(
    tx: &mut Transaction<'_>,
    c: &Conversation,
    prompt: &str,
) -> Result<(Vec<Value>, usize), Error> {
    let mut turns = Vec::new();
    let mut size = serde_json::to_vec(&json!({"role":"user","content":prompt}))
        .map_err(|_| Error::Invalid)?
        .len();
    let mut omitted = 0;
    let mut cutoff = false;
    for turn in c.turns.iter().rev() {
        if cutoff || turn.status != "complete" {
            omitted += 1;
            continue;
        }
        let row = tx
            .get(TABLES[1], &[turn.id.clone().into()])?
            .ok_or(Error::Invalid)?;
        let output: Vec<Value> =
            serde_json::from_str(&text(&row, "output")?).map_err(|_| Error::Invalid)?;
        let mut items = vec![json!({"role":"user","content":turn.user})];
        items.extend(replay(output));
        let bytes = serde_json::to_vec(&items)
            .map_err(|_| Error::Invalid)?
            .len();
        if size + bytes > 192 * 1024 {
            cutoff = true;
            omitted += 1;
            continue;
        }
        size += bytes;
        turns.push(items);
    }
    turns.reverse();
    let mut input = turns.into_iter().flatten().collect::<Vec<_>>();
    input.push(json!({"role":"user","content":prompt}));
    Ok((input, omitted))
}
