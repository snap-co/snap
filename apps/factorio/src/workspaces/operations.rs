//! Workspace requests and admission policy, composed without name-based dispatch.
use super::{
    Command, Config, SESSION_KIND, WORKSPACE_KIND, child_id, document, load, onboard, root,
};
use crate::{
    login,
    operations::{now, request, text},
};
use alloc::{format, string::String, vec, vec::Vec};
use serde_json::{Value, json};
use snap_store::{Error, Transaction};
use snap_transport::{
    Invocation,
    operation::{Context, Definition, Failure, Guard, Handler},
};

pub(crate) fn owner(
    tx: &mut Transaction<'_>,
    call: &Invocation,
    context: &mut Context,
) -> Result<(), Failure> {
    let actor = context.actor.as_deref().ok_or(Error::NotFound)?;
    let workspace = text(&call.input, "workspace")?;
    super::require_owner(tx, workspace, actor)?;
    root(tx, workspace, actor)?;
    Ok(())
}

fn parsed(v: &Value) -> Result<Command, Error> {
    let mut command = v["command"].clone();
    if command["command"] == "start" {
        command["base"] = "".into();
        let id = text(&command, "id")?;
        if command.get("conversation").is_none() {
            command["conversation"] =
                format!("ses_{}", child_id(text(v, "workspace")?, SESSION_KIND, id)).into();
        }
    }
    if command["command"] == "cleanup" {
        command["command"] = "recover".into();
    }
    serde_json::from_value(command).map_err(|_| Error::Invalid)
}

fn command_guard(
    tx: &mut Transaction<'_>,
    call: &Invocation,
    context: &mut Context,
) -> Result<(), Failure> {
    owner(tx, call, context)?;
    let command = parsed(&call.input)?;
    let is_human = if matches!(command, Command::Approve { .. }) {
        login::human(tx, call, context)?;
        true
    } else {
        false
    };
    super::guard(
        tx,
        text(&call.input, "workspace")?,
        context.actor.as_deref().ok_or(Error::NotFound)?,
        is_human,
        now(context)?,
        command,
    )?;
    Ok(())
}

pub(crate) fn declarations(config: Config) -> Vec<Definition> {
    let repository = config.repository.clone();
    let onboarding = config.clone();
    vec![
        request(
            "factorio.repositories",
            Value::is_array,
            snap_store::Data::default(),
            vec![],
            Handler::new(move |_, _, actor, _, _| {
                actor.ok_or(Error::NotFound)?;
                Ok(
                    json!([{ "id":"configured", "path":config.repository, "modules":config.modules }]),
                )
            }),
        ),
        request(
            "factorio.identity",
            |v| v["owner"].is_string(),
            snap_store::Data::default(),
            vec![],
            Handler::new(|_, _, actor, _, _| Ok(json!({"owner":actor.ok_or(Error::NotFound)?}))),
        ),
        login::logout(),
        request(
            "factorio.onboard",
            |v| v.as_object().is_some_and(|o| o.len() == 1) && v["id"].is_string(),
            document().metadata(),
            vec![Guard::new(move |tx, call, context| {
                if text(&call.input, "repository")? != "configured" {
                    return Err(Error::Invalid.into());
                }
                let id = child_id("factorio-host", WORKSPACE_KIND, &repository);
                // Complete Access metadata establishes absence without a cold
                // arbitrary-UUID Document read before onboarding.
                let exists = tx.get("access.resources", &[id.clone().into()])?.is_some();
                if exists {
                    super::require_owner(
                        tx,
                        &id,
                        context.actor.as_deref().ok_or(Error::NotFound)?,
                    )?;
                }
                context.prepared = json!({"id":id, "exists":exists});
                Ok(())
            })],
            Handler::new(move |tx, _, actor, _, context| {
                let actor = actor.ok_or(Error::NotFound)?;
                // One workspace per physical repository keeps claims and ports
                // repository-wide. The owner can reconnect without duplicating it.
                let id = text(&context.prepared, "id")?;
                if context.prepared["exists"] == false {
                    onboard(tx, id, actor, onboarding.clone())?;
                }
                Ok(json!({"id":id}))
            }),
        ),
        request(
            "factorio.workspaces",
            Value::is_array,
            document().metadata(),
            vec![Guard::new(|tx, _, context| {
                let actor = context.actor.as_deref().ok_or(Error::NotFound)?;
                context.prepared = json!(document().access_guard().extent(tx, actor)?);
                Ok(())
            })],
            Handler::new(|tx, _, actor, _, context| {
                let actor = actor.ok_or(Error::NotFound)?;
                let mut roots = Vec::new();
                let ids: Vec<String> =
                    serde_json::from_value(context.prepared.clone()).map_err(|_| Error::Invalid)?;
                for id in ids {
                    let snapshot = document().read(tx, &id, Some(actor))?;
                    if snapshot.kind == WORKSPACE_KIND {
                        roots.push(
                            json!({"id":id,"repository":snapshot.value["config"]["repository"]}),
                        );
                    }
                }
                Ok(json!(roots))
            }),
        ),
        request(
            "factorio.workspace",
            |v| serde_json::from_value::<super::Workspace>(v.clone()).is_ok(),
            document().metadata(),
            vec![Guard::new(owner)],
            Handler::new(|tx, call, actor, _, _| {
                let actor = actor.ok_or(Error::NotFound)?;
                Ok(json!(load(tx, text(&call.input, "workspace")?, actor)?))
            }),
        ),
        request(
            "factorio.command",
            Value::is_null,
            document().metadata().and(login::data()),
            vec![Guard::new(command_guard)],
            Handler::new(|tx, call, actor, _, context| {
                let actor = actor.ok_or(Error::NotFound)?;
                let command = parsed(&call.input)?;
                // Guard already captured human proof; execution does not reauthorize.
                let is_human = matches!(command, Command::Approve { .. });
                super::command(
                    tx,
                    text(&call.input, "workspace")?,
                    actor,
                    is_human,
                    now(context)?,
                    command,
                )?;
                Ok(Value::Null)
            }),
        ),
        login::agent_token(),
    ]
}
