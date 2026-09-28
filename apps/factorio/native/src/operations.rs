//! Guarded application operations. Handlers stage data only; controllers own IO.
use factorio::{Command, documents as graph};
use serde_json::{Value, json};
use snap_document_local::{Host, Request};
use snap_oidc::relying_party as rp;
use snap_store::{Error, Transaction};

pub fn session(tx: &mut Transaction<'_>, bearer: &str) -> Result<(rp::Session, bool), Error> {
    let digest = rp::digest(bearer);
    if let Some(row) = tx.get("factorio.agents", &[digest.clone().into()])? {
        let Some(snap_store::Value::Text(id)) = row.get("session") else {
            return Err(Error::Invalid);
        };
        return Ok((rp::lease(tx, id, crate::now())?, false));
    }
    Ok((rp::lease(tx, &digest, crate::now())?, true))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, Error> {
    v[key].as_str().ok_or(Error::Invalid)
}
fn owner(
    tx: &mut Transaction<'_>,
    actor: Option<&str>,
    v: &Value,
    _: Option<&str>,
) -> Result<(), Error> {
    graph::require_owner(tx, text(v, "workspace")?, actor.ok_or(Error::NotFound)?)?;
    graph::root(tx, text(v, "workspace")?, actor.unwrap())?;
    Ok(())
}
fn human(
    tx: &mut Transaction<'_>,
    _: Option<&str>,
    _: &Value,
    bearer: Option<&str>,
) -> Result<(), Error> {
    if !session(tx, bearer.ok_or(Error::NotFound)?)?.1 {
        return Err(Error::NotFound);
    }
    Ok(())
}
fn parsed(v: &Value) -> Result<Command, Error> {
    let mut command = v["command"].clone();
    if command["command"] == "start" {
        command["base"] = "".into();
        let id = text(&command, "id")?;
        if command.get("conversation").is_none() {
            command["conversation"] = format!(
                "ses_{}",
                graph::child_id(text(v, "workspace")?, graph::SESSION_KIND, id)
            )
            .into();
        }
    }
    if command["command"] == "cleanup" {
        command["command"] = "recover".into();
    }
    serde_json::from_value(command).map_err(|_| Error::Invalid)
}
fn command_guard(
    tx: &mut Transaction<'_>,
    actor: Option<&str>,
    v: &Value,
    bearer: Option<&str>,
) -> Result<(), Error> {
    owner(tx, actor, v, bearer)?;
    let command = parsed(v)?;
    let is_human = if matches!(command, Command::Approve { .. }) {
        human(tx, actor, v, bearer)?;
        true
    } else {
        false
    };
    graph::guard(
        tx,
        text(v, "workspace")?,
        actor.unwrap(),
        is_human,
        crate::now(),
        command,
    )?;
    Ok(())
}

pub fn register(
    mut host: Host<snap_sqlite::Sqlite>,
    config: factorio::Config,
) -> Host<snap_sqlite::Sqlite> {
    for name in [
        "factorio.repositories",
        "factorio.onboard",
        "factorio.workspaces",
        "factorio.workspace",
        "factorio.command",
        "factorio.agent-token",
        "factorio.intake-create",
        "factorio.intake-read",
        "factorio.intake-drafts",
        "factorio.intake-ready",
        "factorio.intake-delete",
    ] {
        let config = config.clone();
        host = host.with_request(Request {
            name: name.into(), identity_required: true,
            input: |v| v.is_object() && v.to_string().len() <= 60000,
            output: match name {
                "factorio.repositories" | "factorio.workspaces" => |v| v.is_array(),
                "factorio.command" | "factorio.intake-delete" => |v| v.is_null(),
                "factorio.onboard" => |v| v.as_object().is_some_and(|o| o.len()==1) && v["id"].is_string(),
                "factorio.agent-token" => |v| v.as_object().is_some_and(|o| o.len()==1) && v["token"].is_string(),
                "factorio.workspace" => |v| serde_json::from_value::<factorio::Workspace>(v.clone()).is_ok(),
                "factorio.intake-read" => |v| serde_json::from_value::<factorio::intake::Intake>(v["intake"].clone()).is_ok() && v["tickets"].is_object() && v["modules"].is_object(),
                _ => |v| serde_json::from_value::<factorio::intake::Intake>(v.clone()).is_ok(),
            }, progress: |_| false,
            guard: match name {
                "factorio.repositories" | "factorio.workspaces" | "factorio.onboard" => |_, actor, _, _| actor.map(|_| ()).ok_or(Error::NotFound),
                "factorio.agent-token" => human,
                "factorio.command" => command_guard,
                _ => owner,
            },
            handler: Box::new(move |tx, invocation, actor, bearer| {
                let actor = actor.ok_or(Error::NotFound)?;
                let v = &invocation.input;
                let workspace = || text(v, "workspace");
                Ok(match name {
                    "factorio.repositories" => json!([{ "id":"configured", "path":config.repository, "modules":config.modules }]),
                    "factorio.onboard" => {
                        if text(v, "repository")? != "configured" { return Err(Error::Invalid); }
                        // One workspace per physical repository keeps claims and ports
                        // repository-wide. The owner may reconnect without duplicating it.
                        let id = graph::child_id("factorio-host", graph::WORKSPACE_KIND, &config.repository);
                        let resource = snap_access::Resource::new("document", &id)?;
                        if graph::document().access.role(tx, &resource, Some(actor), false)?.is_some() {
                            graph::require_owner(tx, &id, actor)?;
                        } else { graph::onboard(tx, &id, actor, config.clone())?; }
                        json!({"id":id})
                    }
                    "factorio.workspaces" => {
                        let mut roots = Vec::new();
                        for id in graph::document().authorized_ids(tx, actor)? {
                            let snapshot = graph::document().read(tx, &id, Some(actor))?;
                            if snapshot.kind == graph::WORKSPACE_KIND { roots.push(json!({"id":id,"repository":snapshot.value["config"]["repository"]})); }
                        }
                        json!(roots)
                    }
                    "factorio.workspace" => json!(graph::load(tx, workspace()?, actor)?),
                    "factorio.command" => {
                        let command = parsed(v)?;
                        let is_human = matches!(command, Command::Approve { .. }); // guard already captured the human proof
                        graph::command(tx, workspace()?, actor, is_human, crate::now(), command)?;
                        Value::Null
                    }
                    "factorio.agent-token" => {
                        // This handler runs immediately after the guarded admission
                        // under the same gate. Read the session ID without reauthorizing.
                        let id = rp::digest(bearer.ok_or(Error::NotFound)?);
                        let token = crate::random();
                        tx.insert("factorio.agents", [("id".into(), rp::digest(&token).into()), ("session".into(), id.into())].into_iter().collect())?;
                        json!({"token":token})
                    }
                    "factorio.intake-create" => json!(graph::create_intake(tx, workspace()?, actor, text(v,"id")?, text(v,"description")?)?),
                    "factorio.intake-read" => {
                        let w = graph::load(tx, workspace()?, actor)?;
                        let item = w.intakes.get(text(v,"id")?).ok_or(Error::NotFound)?;
                        json!({"intake":item,"modules":w.config.modules,"tickets":w.tickets})
                    }
                    "factorio.intake-drafts" => json!(graph::drafts(tx, workspace()?, actor, text(v,"id")?, serde_json::from_value(v["drafts"].clone()).map_err(|_| Error::Invalid)?)?),
                    "factorio.intake-ready" => json!(graph::ready(tx, workspace()?, actor, text(v,"id")?, v["revision"].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or(Error::Invalid)?)?),
                    "factorio.intake-delete" => { graph::delete_intake(tx, workspace()?, actor, text(v,"id")?)?; Value::Null }
                    _ => return Err(Error::Invalid),
                })
            }),
        });
    }
    host
}
