//! Conversation Documents. Clients append messages; Chatty does no model or tool IO.
#![no_std]
extern crate alloc;
use alloc::{string::String, vec, vec::Vec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use snap_access::{Access, Audience, KindDefinition, Role};
use snap_document::{Definition, Intent, Mutation, Registry, Snapshot};
use snap_store::{Error, Transaction};

// Retain the existing tables and schema so old conversations can still be read.
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
    #[serde(default)]
    pub sender: String,
    pub user: String,
    pub text: String,
    pub summary: String,
    pub tools: Vec<Value>,
    pub usage: Value,
    pub status: String,
    pub error: String,
    pub created: i64,
}

fn valid_title(title: &str) -> bool {
    !title.trim().is_empty() && title.chars().count() <= 100
}
fn validate(value: &Value) -> bool {
    serde_json::from_value::<Conversation>(value.clone()).is_ok_and(|c| {
        valid_title(&c.title)
            && c.turns.len() <= 200
            && c.created >= 0
            && c.updated >= 0
            && c.turns.iter().all(|t| {
                !t.user.is_empty()
                    && t.user.len() <= 32768
                    && t.text.len() + t.summary.len() <= 512 * 1024
            })
    })
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub message: String,
    pub created: i64,
}

fn append(value: &Value, args: &Value, actor: &str) -> Result<Value, snap_document::Error> {
    let input: Message =
        serde_json::from_value(args.clone()).map_err(|_| snap_document::Error::Invalid)?;
    if input.id.is_empty()
        || input.id.len() > 100
        || input.created < 0
        || input.message.trim().is_empty()
        || input.message.len() > 32768
    {
        return Err(snap_document::Error::Invalid);
    }
    let mut c: Conversation =
        serde_json::from_value(value.clone()).map_err(|_| snap_document::Error::Invalid)?;
    if let Some(existing) = c.turns.iter().find(|turn| turn.id == input.id) {
        return if existing.user == input.message && existing.sender == actor {
            Ok(value.clone())
        } else {
            Err(snap_document::Error::Invalid)
        };
    }
    if c.turns.len() >= 200 {
        return Err(snap_document::Error::Rejected(
            "Conversation is full".into(),
        ));
    }
    if c.title == "New thread" {
        c.title = input.message.chars().take(60).collect();
    }
    c.updated = c.updated.max(input.created);
    c.active_turn.clear();
    c.turns.push(Turn {
        id: input.id.clone(),
        request_id: input.id,
        sender: actor.into(),
        user: input.message,
        text: String::new(),
        summary: String::new(),
        tools: vec![],
        usage: json!({}),
        status: "complete".into(),
        error: String::new(),
        created: input.created,
    });
    serde_json::to_value(c).map_err(|_| snap_document::Error::Invalid)
}

pub fn registry() -> Registry {
    Registry::new(vec![Definition {
        kind: KIND.into(),
        version: "1".into(),
        validate,
        mutations: vec![
            Mutation {
                name: "send".into(),
                minimum: Role::Editor,
                guard: None,
                apply: append,
            },
            Mutation {
                name: "rename".into(),
                minimum: Role::Editor,
                guard: None,
                apply: |value, args, _| {
                    let title = args["title"]
                        .as_str()
                        .ok_or(snap_document::Error::Invalid)?
                        .trim();
                    if !valid_title(title) {
                        return Err(snap_document::Error::Invalid);
                    }
                    let mut next = value.clone();
                    next["title"] = title.into();
                    Ok(next)
                },
            },
        ],
    }])
    .expect("conversation definitions")
}

pub fn document() -> snap_document::server::Document {
    snap_document::server::Document::new(
        registry(),
        Access::new(vec![KindDefinition::kind("document").unwrap()]).unwrap(),
    )
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub id: String,
    pub title: String,
    pub created: i64,
}

pub fn create(tx: &mut Transaction<'_>, owner: &str, input: &Create) -> Result<(), Error> {
    if owner.is_empty() || !valid_title(&input.title) || input.created < 0 {
        return Err(Error::Invalid);
    }
    if tx.find(TABLES[0], "owner", &[owner.into()])?.len() >= 200 {
        return Err(Error::Constraint);
    }
    document().create(
        tx,
        &Snapshot {
            id: input.id.clone(),
            kind: KIND.into(),
            version: "1".into(),
            revision: 1,
            value: serde_json::to_value(Conversation {
                title: input.title.trim().into(),
                effort: "medium".into(),
                created: input.created,
                updated: input.created,
                active_turn: String::new(),
                turns: vec![],
            })
            .map_err(|_| Error::Invalid)?,
        },
        Audience::Restricted,
        owner,
    )?;
    tx.insert(
        TABLES[0],
        [
            ("id".into(), input.id.clone().into()),
            ("owner".into(), owner.into()),
        ]
        .into_iter()
        .collect(),
    )
}

/// Composite application calls use the same named mutations as Document clients.
pub fn mutate(
    tx: &mut Transaction<'_>,
    owner: &str,
    id: &str,
    mutation: &str,
    args: Value,
) -> Result<(), Error> {
    let result = document().apply(
        tx,
        owner,
        &Intent {
            id: 1,
            document: id.into(),
            version: "1".into(),
            mutation: mutation.into(),
            args,
        },
    )?;
    result
        .completion
        .result
        .map(|_| ())
        .map_err(|_| Error::Invalid)
}
