//! Intake requests share workspace admission and atomic feature transitions.
use crate::{
    operations::{now, request, text},
    workspaces,
};
use alloc::{vec, vec::Vec};
use serde_json::{Value, json};
use snap_store::Error;
use snap_transport::operation::{Definition, Guard, Handler, Validator};
use workspaces::operations::owner;

pub(crate) fn declarations() -> Vec<Definition> {
    let intake: Validator = |v| serde_json::from_value::<super::Intake>(v.clone()).is_ok();
    vec![
        request(
            "factorio.intake-create",
            intake,
            workspaces::document().metadata(),
            vec![Guard::new(owner)],
            Handler::new(|tx, call, actor, _, _| {
                let actor = actor.ok_or(Error::NotFound)?;
                let v = &call.input;
                Ok(json!(super::create(
                    tx,
                    text(v, "workspace")?,
                    actor,
                    text(v, "id")?,
                    text(v, "description")?
                )?))
            }),
        ),
        request(
            "factorio.intake-read",
            |v| {
                serde_json::from_value::<super::Intake>(v["intake"].clone()).is_ok()
                    && v["tickets"].is_object()
                    && v["modules"].is_object()
            },
            workspaces::document().metadata(),
            vec![Guard::new(owner)],
            Handler::new(|tx, call, actor, _, _| {
                let actor = actor.ok_or(Error::NotFound)?;
                let v = &call.input;
                let w = workspaces::load(tx, text(v, "workspace")?, actor)?;
                let item = w.intakes.get(text(v, "id")?).ok_or(Error::NotFound)?;
                Ok(json!({"intake":item,"modules":w.config.modules,"tickets":w.tickets}))
            }),
        ),
        request(
            "factorio.intake-drafts",
            intake,
            workspaces::document().metadata(),
            vec![Guard::new(owner)],
            Handler::new(|tx, call, actor, _, context| {
                let actor = actor.ok_or(Error::NotFound)?;
                let v = &call.input;
                Ok(json!(super::drafts(
                    tx,
                    text(v, "workspace")?,
                    actor,
                    text(v, "id")?,
                    serde_json::from_value(v["drafts"].clone()).map_err(|_| Error::Invalid)?,
                    now(context)?
                )?))
            }),
        ),
        request(
            "factorio.intake-ready",
            intake,
            workspaces::document().metadata(),
            vec![Guard::new(owner)],
            Handler::new(|tx, call, actor, _, _| {
                let actor = actor.ok_or(Error::NotFound)?;
                let v = &call.input;
                Ok(json!(super::ready(
                    tx,
                    text(v, "workspace")?,
                    actor,
                    text(v, "id")?,
                    v["revision"]
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or(Error::Invalid)?
                )?))
            }),
        ),
        request(
            "factorio.intake-delete",
            Value::is_null,
            workspaces::document().metadata(),
            vec![Guard::new(owner)],
            Handler::new(|tx, call, actor, _, _| {
                let actor = actor.ok_or(Error::NotFound)?;
                let v = &call.input;
                super::delete(tx, text(v, "workspace")?, actor, text(v, "id")?)?;
                Ok(Value::Null)
            }),
        ),
    ]
}
