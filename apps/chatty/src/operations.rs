//! Portable thread requests. Bootstrap supplies the clock; carriers own delivery.
use alloc::{vec, vec::Vec};
use serde_json::{Value, json};
use snap_store::Error;
use snap_transport::operation::{Definition, Guard, Handler};

fn field<'a>(input: &'a Value, name: &str) -> Result<&'a str, Error> {
    input[name].as_str().ok_or(Error::Invalid)
}

fn thread(name: &str, inputs: &'static [&'static str], handler: Handler) -> Definition {
    Definition {
        name: name.into(),
        identity_required: true,
        input: |v| v["thread_id"].is_string(),
        output: Value::is_object,
        progress: |_| false,
        error: |_| true,
        data: snap_store::Data::new(&[]),
        inputs,
        guards: vec![Guard::policy(|tx, actor, input, _| {
            let snapshot = crate::document().read(tx, field(input, "thread_id")?, actor)?;
            let resource =
                snap_access::Resource::new("document", &snapshot.id).map_err(|_| Error::Invalid)?;
            if !snap_access::allows(
                snap_document::access::vocabulary().role(tx, &resource, actor, false)?,
                snap_access::Role::Owner,
            ) {
                return Err(Error::Invalid);
            }
            Ok(())
        })],
        handler,
    }
}

pub fn declarations() -> Vec<Definition> {
    vec![
        Definition {
            name: "chatty.create".into(),
            identity_required: true,
            input: |v| serde_json::from_value::<crate::Create>(v.clone()).is_ok(),
            output: Value::is_object,
            progress: |_| false,
            error: |_| true,
            guards: vec![],
            inputs: &[],
            data: snap_store::Data::new(&[]),
            handler: Handler::new(|tx, invocation, owner, _, _| {
                let input = serde_json::from_value::<crate::Create>(invocation.input.clone())
                    .map_err(|_| Error::Invalid)?;
                crate::create(tx, owner.ok_or(Error::Invalid)?, &input)?;
                Ok(json!({"id": input.id}))
            }),
        },
        thread(
            "chatty.send",
            &["clock"],
            Handler::new(|tx, call, owner, _, context| {
                let owner = owner.ok_or(Error::Invalid)?;
                let input = &call.input;
                let id = field(input, "thread_id")?;
                let request_id = field(input, "request_id")?;
                let message = field(input, "message")?;
                let created = context
                    .inputs
                    .get("clock")
                    .and_then(Value::as_i64)
                    .ok_or(Error::Unavailable)?;
                crate::mutate(
                    tx,
                    owner,
                    id,
                    "send",
                    json!({"id":request_id,"message":message,"created":created}),
                )?;
                Ok(json!({"saved":true}))
            }),
        ),
        thread(
            "chatty.rename",
            &[],
            Handler::new(|tx, call, owner, _, _| {
                let owner = owner.ok_or(Error::Invalid)?;
                let input = &call.input;
                crate::mutate(
                    tx,
                    owner,
                    field(input, "thread_id")?,
                    "rename",
                    json!({"title":field(input,"title")?}),
                )?;
                Ok(json!({"saved":true}))
            }),
        ),
        thread(
            "chatty.delete",
            &[],
            Handler::new(|tx, call, owner, _, _| {
                let owner = owner.ok_or(Error::Invalid)?;
                crate::mutate(
                    tx,
                    owner,
                    field(&call.input, "thread_id")?,
                    "document.delete",
                    Value::Null,
                )?;
                Ok(json!({"saved":true}))
            }),
        ),
    ]
}
