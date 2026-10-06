//! All carriers submit arguments to the same authoritative handlers.
use alloc::{vec, vec::Vec};
use serde_json::{Value, json};
use snap_store::Error;
use snap_transport::operation::{Definition, Guard, Handler};
fn field<'a>(input: &'a Value, name: &str) -> Result<&'a str, Error> {
    input[name].as_str().ok_or(Error::Invalid)
}
fn definition(name: &str, inputs: &'static [&'static str], handler: Handler) -> Definition {
    let minimum = if name == "chatty.send" {
        snap_access::Role::Editor
    } else {
        snap_access::Role::Owner
    };
    let guards = if name == "chatty.create" {
        vec![]
    } else {
        vec![Guard::new(move |tx, call, context| {
            crate::require(
                tx,
                context.actor.as_deref().ok_or(Error::NotFound)?,
                field(&call.input, "thread_id")?,
                minimum,
            )?;
            Ok(())
        })]
    };
    Definition {
        name: name.into(),
        identity_required: true,
        input: Value::is_object,
        output: Value::is_object,
        progress: |_| false,
        error: |_| true,
        data: crate::data(),
        inputs,
        guards,
        handler,
    }
}
pub fn declarations() -> Vec<Definition> {
    vec![
        definition(
            "chatty.create",
            &["clock"],
            Handler::new(|tx, call, actor, _, context| {
                let input: crate::Create =
                    serde_json::from_value(call.input.clone()).map_err(|_| Error::Invalid)?;
                crate::create(
                    tx,
                    actor.ok_or(Error::NotFound)?,
                    &input,
                    context.inputs["clock"].as_i64().ok_or(Error::Unavailable)?,
                )?;
                Ok(json!({"id":input.id}))
            }),
        ),
        definition(
            "chatty.send",
            &["clock"],
            Handler::new(|tx, call, actor, _, context| {
                let input = &call.input;
                let message = crate::send(
                    tx,
                    actor.ok_or(Error::NotFound)?,
                    field(input, "thread_id")?,
                    field(input, "request_id")?,
                    field(input, "message")?,
                    context.inputs["clock"].as_i64().ok_or(Error::Unavailable)?,
                )?;
                Ok(json!(message))
            }),
        ),
        definition(
            "chatty.rename",
            &[],
            Handler::new(|tx, call, actor, _, _| {
                let id = field(&call.input, "thread_id")?;
                crate::require(
                    tx,
                    actor.ok_or(Error::NotFound)?,
                    id,
                    snap_access::Role::Owner,
                )?;
                let title = field(&call.input, "title")?.trim();
                crate::bounded(title, 200)?;
                tx.update(
                    crate::THREADS,
                    &[id.into()],
                    [("title".into(), title.into())].into_iter().collect(),
                )?;
                Ok(json!({"saved":true}))
            }),
        ),
        definition(
            "chatty.member",
            &[],
            Handler::new(|tx, call, actor, _, _| {
                let actor = actor.ok_or(Error::NotFound)?;
                let resource =
                    snap_access::Resource::new(crate::KIND, field(&call.input, "thread_id")?)?;
                crate::require(tx, actor, &resource.id, snap_access::Role::Owner)?;
                let identity = field(&call.input, "identity")?;
                let role = match call.input.get("role") {
                    Some(Value::Null) => None,
                    Some(Value::String(role)) if role == "editor" => {
                        Some(snap_access::Role::Editor)
                    }
                    Some(Value::String(role)) if role == "viewer" => {
                        Some(snap_access::Role::Viewer)
                    }
                    _ => return Err(Error::Invalid),
                };
                // This prototype has one owner. Membership cannot demote that owner.
                if identity == actor {
                    return Err(Error::Invalid);
                }
                let mut changes = snap_access::ChangeSet::new(snap_access::Actor::identity(actor)?);
                changes.grants.push(snap_access::GrantChange {
                    resource,
                    identity: identity.into(),
                    role,
                });
                crate::access().change(tx, &changes)?;
                Ok(json!({"saved":true}))
            }),
        ),
        definition(
            "chatty.delete",
            &[],
            Handler::new(|tx, call, actor, _, _| {
                let id = field(&call.input, "thread_id")?;
                crate::require(
                    tx,
                    actor.ok_or(Error::NotFound)?,
                    id,
                    snap_access::Role::Owner,
                )?;
                for row in tx.find(crate::MESSAGES, "primary", &[id.into()])? {
                    tx.delete(crate::MESSAGES, &[id.into(), row["sequence"].clone()])?;
                }
                tx.delete(crate::THREADS, &[id.into()])?;
                crate::access().remove(tx, &snap_access::Resource::new(crate::KIND, id)?, None)?;
                Ok(json!({"saved":true}))
            }),
        ),
    ]
}
