//! Shared messaging through ordinary Store rows and Transport operations.
//! Humans and external agents are clients. Chatty performs no model or tool IO.
#![no_std]
extern crate alloc;
pub mod client;
pub mod operations;
use alloc::{string::String, sync::Arc, vec, vec::Vec};
use snap_access::{Access, Audience, DirectGrant, KindDefinition, Resource, Role, TransferPolicy};
use snap_store::{Catalog, Data, Error, Row, Transaction, Value};

pub const MIGRATION: &str = include_str!("../migrations/0001_chatty.toml");
pub const TABLES: [&str; 2] = ["chatty.threads", "chatty.messages"];
pub const THREADS: &str = TABLES[0];
pub const MESSAGES: &str = TABLES[1];
pub const KIND: &str = "chatty-thread";
pub fn data() -> Data {
    Data::new(&TABLES).and(Data::new(&snap_access::TABLES))
}
pub fn access() -> Access {
    Access::new(vec![
        KindDefinition::new(KIND, TransferPolicy::Forbidden).expect("thread kind"),
    ])
    .expect("thread access")
}
pub(crate) fn text<'a>(row: &'a Row, name: &str) -> Result<&'a str, Error> {
    match row.get(name) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(Error::Invalid),
    }
}
fn number(row: &Row, name: &str) -> Result<i64, Error> {
    match row.get(name) {
        Some(Value::Integer(value)) => Ok(*value),
        _ => Err(Error::Invalid),
    }
}
fn bounded(value: &str, max: usize) -> Result<(), Error> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        Err(Error::Invalid)
    } else {
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Thread {
    pub id: String,
    pub title: String,
    pub created: i64,
    pub updated: i64,
    pub next_sequence: i64,
}
impl Thread {
    pub fn from_row(row: &Row) -> Result<Self, Error> {
        Ok(Self {
            id: text(row, "id")?.into(),
            title: text(row, "title")?.into(),
            created: number(row, "created")?,
            updated: number(row, "updated")?,
            next_sequence: number(row, "next_sequence")?,
        })
    }
    pub fn read(tx: &mut Transaction<'_>, id: &str) -> Result<Self, Error> {
        Self::from_row(&tx.get(THREADS, &[id.into()])?.ok_or(Error::NotFound)?)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub thread: String,
    pub sequence: i64,
    pub sender: String,
    pub request: String,
    pub body: String,
    pub created: i64,
}
impl Message {
    pub fn from_row(row: &Row) -> Result<Self, Error> {
        Ok(Self {
            thread: text(row, "thread")?.into(),
            sequence: number(row, "sequence")?,
            sender: text(row, "sender")?.into(),
            request: text(row, "request")?.into(),
            body: text(row, "body")?.into(),
            created: number(row, "created")?,
        })
    }
}
pub fn require(
    tx: &mut Transaction<'_>,
    actor: &str,
    thread: &str,
    minimum: Role,
) -> Result<(), Error> {
    if !snap_access::allows(
        access().role(tx, &Resource::new(KIND, thread)?, Some(actor), true)?,
        minimum,
    ) {
        return Err(Error::NotFound);
    }
    Ok(())
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub id: String,
    pub title: String,
}
pub fn create(
    tx: &mut Transaction<'_>,
    actor: &str,
    input: &Create,
    now: i64,
) -> Result<(), Error> {
    bounded(&input.title, 200)?;
    if now < 0 {
        return Err(Error::Invalid);
    }
    let resource = Resource::new(KIND, &input.id)?;
    tx.insert(
        THREADS,
        Row::from([
            ("id".into(), input.id.clone().into()),
            ("title".into(), input.title.trim().into()),
            ("created".into(), now.into()),
            ("updated".into(), now.into()),
            ("next_sequence".into(), 1.into()),
        ]),
    )?;
    access().register(
        tx,
        &resource,
        Audience::Restricted,
        &[DirectGrant::new(actor, Role::Owner)?],
        None,
    )?;
    Ok(())
}
/// Requests are idempotent within thread and sender, independently of invocation
/// IDs. Reusing a request for different content fails, including after restart.
pub fn send(
    tx: &mut Transaction<'_>,
    actor: &str,
    thread: &str,
    request: &str,
    body: &str,
    now: i64,
) -> Result<Message, Error> {
    require(tx, actor, thread, Role::Editor)?;
    bounded(request, 128)?;
    bounded(body, 32768)?;
    if now < 0 {
        return Err(Error::Invalid);
    }
    if let Some(row) = tx
        .find(
            MESSAGES,
            "request",
            &[thread.into(), actor.into(), request.into()],
        )?
        .first()
    {
        let message = Message::from_row(row)?;
        if message.body != body {
            return Err(Error::Constraint);
        }
        return Ok(message);
    }
    let current = Thread::read(tx, thread)?;
    let sequence = current.next_sequence;
    let next = sequence.checked_add(1).ok_or(Error::Invalid)?;
    tx.insert(
        MESSAGES,
        Row::from([
            ("thread".into(), thread.into()),
            ("sequence".into(), sequence.into()),
            ("sender".into(), actor.into()),
            ("request".into(), request.into()),
            ("body".into(), body.into()),
            ("created".into(), now.into()),
        ]),
    )?;
    tx.update(
        THREADS,
        &[thread.into()],
        Row::from([
            ("updated".into(), now.max(current.updated).into()),
            ("next_sequence".into(), next.into()),
        ]),
    )?;
    Ok(Message {
        thread: thread.into(),
        sequence,
        sender: actor.into(),
        request: request.into(),
        body: body.into(),
        created: now,
    })
}
pub fn catalog() -> Catalog {
    use snap_store::{Column, Kind, Table};
    let table = |name: &str, columns: &[(&str, Kind)], primary: &[&str]| Table {
        name: name.into(),
        columns: columns
            .iter()
            .map(|(name, kind)| Column {
                name: (*name).into(),
                kind: *kind,
            })
            .collect(),
        primary: primary.iter().map(|name| (*name).into()).collect(),
        indexes: vec![],
        foreign: vec![],
    };
    Catalog::new(vec![
        table(
            THREADS,
            &[
                ("id", Kind::Text),
                ("title", Kind::Text),
                ("created", Kind::Integer),
                ("updated", Kind::Integer),
                ("next_sequence", Kind::Integer),
            ],
            &["id"],
        ),
        table(
            MESSAGES,
            &[
                ("thread", Kind::Text),
                ("sequence", Kind::Integer),
                ("sender", Kind::Text),
                ("request", Kind::Text),
                ("body", Kind::Text),
                ("created", Kind::Integer),
            ],
            &["thread", "sequence"],
        ),
    ])
    .expect("Chatty replica schema")
}
pub fn replication() -> Arc<snap_transport::replication::Registry> {
    let mut declarations = Vec::new();
    for table in catalog().tables {
        let name = table.name.clone();
        declarations.push(
            snap_transport::replication::Declaration::new(table, data(), |tx, actor, key| {
                let Some(Value::Text(thread)) = key.first() else {
                    return Err(Error::Invalid);
                };
                Ok(snap_access::allows(
                    access().role(tx, &Resource::new(KIND, thread)?, Some(actor), true)?,
                    Role::Viewer,
                ))
            })
            .with_collection(move |tx, actor, parameters| {
                if name == THREADS {
                    if !parameters.is_null() {
                        return Err(Error::Invalid);
                    }
                    Ok(access()
                        .accessible(tx, Some(actor), true)?
                        .into_iter()
                        .filter(|item| item.resource.kind == KIND)
                        .map(|item| vec![item.resource.id.into()])
                        .collect())
                } else {
                    let thread = parameters.as_str().ok_or(Error::Invalid)?;
                    Resource::new(KIND, thread)?;
                    match require(tx, actor, thread, Role::Viewer) {
                        Ok(()) => {}
                        Err(Error::NotFound) => return Ok(Default::default()),
                        Err(error) => return Err(error),
                    }
                    Ok(tx
                        .find(MESSAGES, "primary", &[thread.into()])?
                        .iter()
                        .map(|row| Ok(vec![thread.into(), number(row, "sequence")?.into()]))
                        .collect::<Result<_, Error>>()?)
                }
            }),
        );
    }
    Arc::new(snap_transport::replication::Registry::new(declarations).expect("Chatty replication"))
}
