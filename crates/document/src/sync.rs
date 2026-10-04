//! Document extent and replication bindings. The host owns observers, connections,
//! residency and output queues. These callbacks describe only Document wire rules.
use crate::{Completion, Manifest, Replication, ServerMessage, Snapshot, server::Document};
use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};
use snap_store::Error;
use snap_transport::{Value, subscription::Definition};

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Publication {
    pub completion: Completion,
    pub replication: Option<Replication>,
}
pub fn binding(document: Arc<Document>) -> Definition {
    let extent = document.clone();
    let read = document.clone();
    Definition {
        topic: crate::wire::KIND.into(),
        table: crate::server::TABLES[0],
        data: document.metadata(),
        extent: Box::new(move |tx, actor| {
            Ok(extent
                .access_guard()
                .extent(tx, actor)?
                .into_iter()
                .map(|id| alloc::vec![id.into()])
                .collect())
        }),
        read: Box::new(move |tx, lifetime, actor| {
            let state = read
                .access_guard()
                .manifest(tx, lifetime, actor, &Manifest::default())?;
            serde_json::to_value(state.documents).map_err(|_| Error::Invalid)
        }),
        changes: Box::new(changes),
        origin: Box::new(|value| {
            let Some(publication) = publication(value)? else {
                return Ok(None);
            };
            if publication.completion.result.is_ok() {
                Ok(Some(encode(ServerMessage::Committed(
                    publication.completion,
                ))?))
            } else {
                Ok(None)
            }
        }),
        filter: Box::new(|value, allowed| {
            let mut message = serde_json::from_value(value.clone()).map_err(|_| Error::Invalid)?;
            let allowed: BTreeSet<_> = allowed
                .iter()
                .filter_map(|key| match key.as_slice() {
                    [snap_store::Value::Text(id)] => Some(id.clone()),
                    _ => None,
                })
                .collect();
            if !filter(&mut message, &allowed) {
                return Ok(false);
            }
            *value = encode(message)?;
            Ok(true)
        }),
        expire: Box::new(move |tx, lifetime| document.expire(tx, lifetime)),
        reset: serde_json::to_value(ServerMessage::Reset).expect("document reset"),
    }
}
fn publication(value: &Value) -> Result<Option<Publication>, Error> {
    if value.is_null() {
        Ok(None)
    } else {
        serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|_| Error::Invalid)
    }
}
fn encode(message: ServerMessage) -> Result<Value, Error> {
    serde_json::to_value(message).map_err(|_| Error::Invalid)
}
fn changes(prior: &Value, state: &Value, data: &Value, origin: bool) -> Result<Vec<Value>, Error> {
    let prior: Vec<Snapshot> = if prior.is_null() {
        Vec::new()
    } else {
        serde_json::from_value(prior.clone()).map_err(|_| Error::Invalid)?
    };
    let documents: Vec<Snapshot> =
        serde_json::from_value(state.clone()).map_err(|_| Error::Invalid)?;
    let held: BTreeMap<_, _> = prior.iter().map(|s| (&s.id, s)).collect();
    let desired: BTreeMap<_, _> = documents.iter().map(|s| (&s.id, s)).collect();
    let publication = publication(data)?;
    let replication = publication.as_ref().and_then(|p| p.replication.as_ref());
    let mut messages = Vec::new();
    let removed: Vec<_> = held
        .keys()
        .filter(|id| !desired.contains_key(*id))
        .map(|id| (*id).clone())
        .collect();
    if !removed.is_empty() {
        messages.push(encode(ServerMessage::Removed(removed))?);
    }
    let gained = desired.keys().any(|id| !held.contains_key(id));
    let replacement = gained
        || desired.iter().any(|(id, snapshot)| {
            held.get(id) != Some(snapshot)
                && replication.is_none_or(|intent| intent.intent.document != **id)
        });
    if replacement {
        messages.push(encode(ServerMessage::Holdings(documents))?);
    } else if let Some(intent) = replication
        && !origin
        && desired.contains_key(&intent.intent.document)
    {
        messages.push(encode(ServerMessage::Replication(intent.clone()))?);
    }
    Ok(messages)
}
fn filter_completion(completion: &mut Completion, allowed: &BTreeSet<alloc::string::String>) {
    if !allowed.contains(&completion.document) && completion.result.is_ok() {
        completion.result = Ok(None);
    }
}
fn filter(message: &mut ServerMessage, allowed: &BTreeSet<alloc::string::String>) -> bool {
    match message {
        ServerMessage::Completed(c) => filter_completion(c, allowed),
        ServerMessage::Manifest(state) => {
            state.unchanged.retain(|h| allowed.contains(&h.document));
            state.documents.retain(|s| allowed.contains(&s.id));
            for c in &mut state.completed {
                filter_completion(c, allowed);
            }
        }
        ServerMessage::Holdings(documents) => documents.retain(|s| allowed.contains(&s.id)),
        ServerMessage::Replication(intent) => return allowed.contains(&intent.intent.document),
        _ => {}
    }
    true
}
