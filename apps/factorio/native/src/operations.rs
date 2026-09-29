//! Guarded application operations. Handlers stage data only; controllers own IO.
use factorio::{Command, documents as graph};
use serde_json::{Value, json};
use snap_document_local::{Host, Request};
use snap_oidc::relying_party as rp;
use snap_store::{Error, Transaction};

pub fn session(tx: &mut Transaction<'_>, bearer: &str) -> Result<(rp::Session, bool), Error> {
    let digest = rp::digest(bearer);
    if let Some(row) = tx.get("factorio.cli", &[digest.clone().into()])? {
        let (Some(snap_store::Value::Text(id)), Some(snap_store::Value::Integer(expires))) =
            (row.get("session"), row.get("expires"))
        else {
            return Err(Error::Invalid);
        };
        if *expires <= crate::now() {
            return Err(Error::NotFound);
        }
        return Ok((rp::lease(tx, id, crate::now())?, false));
    }
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
    origin: String,
) -> Host<snap_sqlite::Sqlite> {
    for name in ["factorio.login-start", "factorio.login-finish"] {
        let origin = origin.clone();
        host = host.with_preconnection_request(Request {
            name: name.into(), identity_required: false,
            input: |v| v.is_object(), output: |v| v.is_object() || v.is_null(), progress: |_| false,
            guard: |_,_,_,_|Ok(()),
            handler: Box::new(move |tx, call, _, _| {
                if name == "factorio.login-start" {
                    let rows = tx.find("factorio.cli_login", "primary", &[])?;
                    let mut active = 0;
                    for row in rows {
                        if let (Some(snap_store::Value::Text(id)),Some(snap_store::Value::Integer(expires))) = (row.get("id"),row.get("expires")) {
                            if *expires <= crate::now() { tx.delete("factorio.cli_login", &[id.clone().into()])?; } else {active += 1;}
                        }
                    }
                    if active >= 128 { return Err(Error::Unavailable); }
                    let code = crate::random(); let proof = crate::random(); let expires = crate::now()+300;
                    tx.insert("factorio.cli_login",[("id".into(),code.clone().into()),("proof".into(),rp::digest(&proof).into()),("session".into(),"".into()),("expires".into(),expires.into())].into_iter().collect())?;
                    Ok(json!({"code":code,"proof":proof,"expires":expires,"url":format!("{origin}/auth/cli/{code}")}))
                } else {
                    let input = &call.input;
                    let code = text(input,"code")?;
                    let row = tx.get("factorio.cli_login", &[code.into()])?.ok_or(Error::NotFound)?;
                    let (Some(snap_store::Value::Text(proof)),Some(snap_store::Value::Text(session)),Some(snap_store::Value::Integer(expires))) = (row.get("proof"),row.get("session"),row.get("expires")) else {return Err(Error::Invalid);};
                    if *expires <= crate::now() || !rp::same_secret(proof,&rp::digest(text(input,"proof")?)) {return Err(Error::NotFound);}
                    if session.is_empty() { return Ok(Value::Null); }
                    let s = rp::lease(tx,session,crate::now())?;
                    let token = crate::random(); let expires = (crate::now()+1800).min(s.expires).min(s.tokens.access_expires);
                    tx.insert("factorio.cli",[("id".into(),rp::digest(&token).into()),("session".into(),session.clone().into()),("expires".into(),expires.into())].into_iter().collect())?;
                    tx.delete("factorio.cli_login", &[code.into()])?;
                    Ok(json!({"bearer":token,"expires":expires,"owner":s.owner}))
                }
            }),
        }, &["factorio.cli_login", "factorio.cli", "oidc_rp.sessions"]);
    }
    host = host.with_preconnection_request(
        Request {
            name: "factorio.login".into(),
            identity_required: true,
            input: |v| v.is_null(),
            output: |v| v["bearer"].is_string() && v["expires"].is_i64() && v["owner"].is_string(),
            progress: |_| false,
            guard: |tx, _, _, bearer| {
                // Only an ordinary agent token may delegate CLI authority. A CLI
                // credential cannot extend itself or borrow browser refresh rights.
                let digest = rp::digest(bearer.ok_or(Error::NotFound)?);
                tx.get("factorio.agents", &[digest.into()])?
                    .ok_or(Error::NotFound)?;
                Ok(())
            },
            handler: Box::new(|tx, _, _, bearer| {
                let (s, human) = session(tx, bearer.ok_or(Error::NotFound)?)?;
                if human {
                    return Err(Error::NotFound);
                }
                let token = crate::random();
                let expires = (crate::now() + 1800)
                    .min(s.expires)
                    .min(s.tokens.access_expires);
                tx.insert(
                    "factorio.cli",
                    [
                        ("id".into(), rp::digest(&token).into()),
                        ("session".into(), s.id.into()),
                        ("expires".into(), expires.into()),
                    ]
                    .into_iter()
                    .collect(),
                )?;
                Ok(json!({"bearer":token,"expires":expires,"owner":s.owner}))
            }),
        },
        &["factorio.agents", "factorio.cli", "oidc_rp.sessions"],
    );
    for name in [
        "factorio.repositories",
        "factorio.identity",
        "factorio.logout",
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
                "factorio.identity" => |v| v["owner"].is_string(),
                "factorio.logout" => |v| v.is_null(),
                "factorio.repositories" | "factorio.workspaces" => |v| v.is_array(),
                "factorio.command" | "factorio.intake-delete" => |v| v.is_null(),
                "factorio.onboard" => |v| v.as_object().is_some_and(|o| o.len()==1) && v["id"].is_string(),
                "factorio.agent-token" => |v| v.as_object().is_some_and(|o| o.len()==1) && v["token"].is_string(),
                "factorio.workspace" => |v| serde_json::from_value::<factorio::Workspace>(v.clone()).is_ok(),
                "factorio.intake-read" => |v| serde_json::from_value::<factorio::intake::Intake>(v["intake"].clone()).is_ok() && v["tickets"].is_object() && v["modules"].is_object(),
                _ => |v| serde_json::from_value::<factorio::intake::Intake>(v.clone()).is_ok(),
            }, progress: |_| false,
            guard: match name {
                "factorio.repositories" | "factorio.workspaces" | "factorio.onboard" | "factorio.identity" | "factorio.logout" => |_, actor, _, _| actor.map(|_| ()).ok_or(Error::NotFound),
                "factorio.agent-token" => human,
                "factorio.command" => command_guard,
                _ => owner,
            },
            handler: Box::new(move |tx, invocation, actor, bearer| {
                let actor = actor.ok_or(Error::NotFound)?;
                let v = &invocation.input;
                let workspace = || text(v, "workspace");
                Ok(match name {
                    "factorio.identity" => json!({"owner":actor,"human":session(tx,bearer.ok_or(Error::NotFound)?)?.1}),
                    "factorio.logout" => {
                        let id = rp::digest(bearer.ok_or(Error::NotFound)?);
                        if tx.get("factorio.cli", &[id.clone().into()])?.is_some() { tx.delete("factorio.cli", &[id.into()])?; }
                        else if tx.get("factorio.agents", &[id.clone().into()])?.is_some() { tx.delete("factorio.agents", &[id.into()])?; }
                        else { return Err(Error::NotFound); }
                        Value::Null
                    },
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
                    "factorio.intake-drafts" => json!(graph::drafts(tx, workspace()?, actor, text(v,"id")?, serde_json::from_value(v["drafts"].clone()).map_err(|_| Error::Invalid)?, crate::now())?),
                    "factorio.intake-ready" => json!(graph::ready(tx, workspace()?, actor, text(v,"id")?, v["revision"].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or(Error::Invalid)?)?),
                    "factorio.intake-delete" => { graph::delete_intake(tx, workspace()?, actor, text(v,"id")?)?; Value::Null }
                    _ => return Err(Error::Invalid),
                })
            }),
        });
    }
    host
}
