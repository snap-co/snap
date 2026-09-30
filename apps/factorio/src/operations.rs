//! Guarded application operations. Handlers stage data only; controllers own IO.
use crate::{Command, workspaces as graph};
use alloc::{
    borrow::ToOwned,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use serde_json::{Value, json};
use snap_oidc::relying_party as rp;
use snap_store::{Error, Transaction};
use snap_transport::operation::{Context, Definition as Request, Failure, Guard};

/// Local login credentials last independently of short-lived upstream access
/// tokens, but never extend the backing OAuth session or its refresh grant.
const CLI_LOGIN_SECONDS: i64 = 30 * 24 * 60 * 60;

pub fn session_id(
    tx: &mut Transaction<'_>,
    bearer: &str,
    now: i64,
) -> Result<(String, bool), Error> {
    let digest = rp::digest(bearer);
    if let Some(row) = tx.get("factorio.cli", &[digest.clone().into()])? {
        let (Some(snap_store::Value::Text(id)), Some(snap_store::Value::Integer(expires))) =
            (row.get("session"), row.get("expires"))
        else {
            return Err(Error::Invalid);
        };
        if *expires <= now {
            return Err(Error::NotFound);
        }
        return Ok((id.clone(), false));
    }
    if let Some(row) = tx.get("factorio.agents", &[digest.clone().into()])? {
        let Some(snap_store::Value::Text(id)) = row.get("session") else {
            return Err(Error::Invalid);
        };
        return Ok((id.clone(), false));
    }
    Ok((digest, true))
}

pub fn session(
    tx: &mut Transaction<'_>,
    bearer: &str,
    now: i64,
) -> Result<(rp::Session, bool), Error> {
    let (id, human) = session_id(tx, bearer, now)?;
    Ok((rp::lease(tx, &id, now)?, human))
}

/// Recovery retention cannot authorize operations or extend either login expiry.
pub fn retained(tx: &mut Transaction<'_>, bearer: &str, now: i64) -> Result<String, Error> {
    let (id, _) = session_id(tx, bearer, now)?;
    rp::retained(tx, &id, now).map(|s| s.owner)
}
fn now(context: &Context) -> Result<i64, Error> {
    context
        .inputs
        .get("clock")
        .and_then(Value::as_i64)
        .ok_or(Error::Unavailable)
}
fn entropy(context: &Context, key: &str) -> Result<String, Error> {
    context
        .inputs
        .get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or(Error::Unavailable)
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
    _: &snap_transport::Invocation,
    context: &mut Context,
) -> Result<(), Failure> {
    if !session(
        tx,
        context.bearer.as_deref().ok_or(Error::NotFound)?,
        now(context)?,
    )?
    .1
    {
        return Err(Error::NotFound.into());
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
    invocation: &snap_transport::Invocation,
    context: &mut Context,
) -> Result<(), Failure> {
    let v = &invocation.input;
    owner(tx, context.actor.as_deref(), v, context.bearer.as_deref())?;
    let command = parsed(v)?;
    let is_human = if matches!(command, Command::Approve { .. }) {
        human(tx, invocation, context)?;
        true
    } else {
        false
    };
    graph::guard(
        tx,
        text(v, "workspace")?,
        context.actor.as_deref().ok_or(Error::NotFound)?,
        is_human,
        now(context)?,
        command,
    )?;
    Ok(())
}

fn login_finish(
    tx: &mut Transaction<'_>,
    call: &snap_transport::Invocation,
    context: &mut Context,
) -> Result<(), Failure> {
    let code = text(&call.input, "code")?;
    let row = tx
        .get("factorio.cli_login", &[code.into()])?
        .ok_or(Error::NotFound)?;
    let (
        Some(snap_store::Value::Text(proof)),
        Some(snap_store::Value::Text(session)),
        Some(snap_store::Value::Integer(expires)),
    ) = (row.get("proof"), row.get("session"), row.get("expires"))
    else {
        return Err(Error::Invalid.into());
    };
    if *expires <= now(context)?
        || !rp::same_secret(proof, &rp::digest(text(&call.input, "proof")?))
    {
        return Err(Error::NotFound.into());
    }
    context.prepared = if session.is_empty() {
        json!({"pending":true})
    } else {
        let lease = rp::lease(tx, session, now(context)?)?;
        json!({"code":code, "session":session, "expires":lease.expires, "owner":lease.owner})
    };
    Ok(())
}

pub fn declarations(config: crate::Config, origin: String) -> crate::Application {
    let mut app = crate::Application {
        requests: vec![],
        preconnection: vec![],
    };
    for name in ["factorio.login-start", "factorio.login-finish"] {
        let origin = origin.clone();
        app.preconnection.push(Request {
            name: name.into(), identity_required: false,
            input: |v| v.is_object(), output: |v| v.is_object() || v.is_null(), progress: |_| false,
            error: |_| true,
            guards: if name == "factorio.login-finish" { vec![Guard::new(login_finish)] } else { vec![] },
            tables: &["factorio.cli_login", "factorio.cli", "oidc_rp.sessions"],
            inputs: if name == "factorio.login-start" { &["clock", "entropy", "proof"] } else { &["clock", "entropy"] },
            handler: snap_transport::operation::Handler::new(move |tx, _, _, _, context| {
                let now = now(context)?;
                if name == "factorio.login-start" {
                    let rows = tx.find("factorio.cli_login", "primary", &[])?;
                    let mut active = 0;
                    for row in rows {
                        if let (Some(snap_store::Value::Text(id)),Some(snap_store::Value::Integer(expires))) = (row.get("id"),row.get("expires")) {
                            if *expires <= now { tx.delete("factorio.cli_login", &[id.clone().into()])?; } else {active += 1;}
                        }
                    }
                    if active >= 128 { return Err(Error::Unavailable); }
                    let code = entropy(context, "entropy")?; let proof = entropy(context, "proof")?; let expires = now+300;
                    tx.insert("factorio.cli_login",[("id".into(),code.clone().into()),("proof".into(),rp::digest(&proof).into()),("session".into(),"".into()),("expires".into(),expires.into())].into_iter().collect())?;
                    Ok(json!({"code":code,"proof":proof,"expires":expires,"url":format!("{origin}/auth/cli/{code}")}))
                } else {
                    let prepared = &context.prepared;
                    if prepared["pending"] == true { return Ok(Value::Null); }
                    let code = text(prepared,"code")?;
                    let session = text(prepared,"session")?;
                    let session_expires = prepared["expires"].as_i64().ok_or(Error::Invalid)?;
                    let token = entropy(context, "entropy")?; let expires = (now+CLI_LOGIN_SECONDS).min(session_expires);
                    tx.insert("factorio.cli",[("id".into(),rp::digest(&token).into()),("session".into(),session.into()),("expires".into(),expires.into())].into_iter().collect())?;
                    tx.delete("factorio.cli_login", &[code.into()])?;
                    Ok(json!({"bearer":token,"expires":expires,"owner":prepared["owner"]}))
                }
            }),
        });
    }
    app.preconnection.push(Request {
        name: "factorio.login".into(),
        identity_required: true,
        input: |v| v.is_null(),
        output: |v| v["bearer"].is_string() && v["expires"].is_i64() && v["owner"].is_string(),
        progress: |_| false,
        tables: &["factorio.agents", "factorio.cli", "oidc_rp.sessions"],
        inputs: &["clock", "entropy"],
        error: |_| true,
        guards: vec![Guard::new(|tx, _, context| {
            // Only an ordinary agent token may delegate CLI authority. A CLI
            // credential cannot delegate or extend its backing OAuth grant.
            let bearer = context.bearer.as_deref().ok_or(Error::NotFound)?;
            let digest = rp::digest(bearer);
            tx.get("factorio.agents", &[digest.into()])?
                .ok_or(Error::NotFound)?;
            let (session, _) = session(tx, bearer, now(context)?)?;
            context.prepared =
                json!({"id":session.id,"expires":session.expires,"owner":session.owner});
            Ok(())
        })],
        handler: snap_transport::operation::Handler::new(|tx, _, _, _, context| {
            let s = &context.prepared;
            let id = text(s, "id")?.to_owned();
            let owner = text(s, "owner")?.to_owned();
            let session_expires = s["expires"].as_i64().ok_or(Error::Invalid)?;
            let token = entropy(context, "entropy")?;
            let expires = (now(context)? + CLI_LOGIN_SECONDS).min(session_expires);
            tx.insert(
                "factorio.cli",
                [
                    ("id".into(), rp::digest(&token).into()),
                    ("session".into(), id.into()),
                    ("expires".into(), expires.into()),
                ]
                .into_iter()
                .collect(),
            )?;
            Ok(json!({"bearer":token,"expires":expires,"owner":owner}))
        }),
    });
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
        app.requests.push(Request {
            name: name.into(), identity_required: true,
            input: |v| v.is_object() && v.to_string().len() <= 60000,
            output: match name {
                "factorio.identity" => |v| v["owner"].is_string(),
                "factorio.logout" => |v| v.is_null(),
                "factorio.repositories" | "factorio.workspaces" => |v| v.is_array(),
                "factorio.command" | "factorio.intake-delete" => |v| v.is_null(),
                "factorio.onboard" => |v| v.as_object().is_some_and(|o| o.len()==1) && v["id"].is_string(),
                "factorio.agent-token" => |v| v.as_object().is_some_and(|o| o.len()==1) && v["token"].is_string(),
                "factorio.workspace" => |v| serde_json::from_value::<crate::Workspace>(v.clone()).is_ok(),
                "factorio.intake-read" => |v| serde_json::from_value::<crate::intake::Intake>(v["intake"].clone()).is_ok() && v["tickets"].is_object() && v["modules"].is_object(),
                _ => |v| serde_json::from_value::<crate::intake::Intake>(v.clone()).is_ok(),
            }, progress: |_| false,
            error: |_| true,
            // Document values follow the workspace extent. Policy metadata and
            // credential rows are explicit complete-table requirements.
            tables: match name {
                "factorio.identity" | "factorio.repositories" => &[],
                "factorio.logout" => &["factorio.cli", "factorio.agents"],
                "factorio.agent-token" => &["factorio.cli", "factorio.agents", "oidc_rp.sessions"],
                "factorio.command" => &["access.resources", "access.grants", "access.links", "document.lifecycle", "factorio.cli", "factorio.agents", "oidc_rp.sessions"],
                _ => &["access.resources", "access.grants", "access.links", "document.lifecycle"],
            },
            inputs: if name == "factorio.agent-token" { &["clock", "entropy"] } else { &["clock"] },
            guards: match name {
                "factorio.repositories" | "factorio.identity" | "factorio.logout" => vec![],
                "factorio.workspaces" => vec![Guard::new(|tx, _, context| {
                    let actor = context.actor.as_deref().ok_or(Error::NotFound)?;
                    context.prepared = json!(graph::document().access_guard().extent(tx, actor)?);
                    Ok(())
                })],
                "factorio.onboard" => {
                    let repository = config.repository.clone();
                    vec![Guard::new(move |tx, call, context| {
                        if text(&call.input, "repository")? != "configured" { return Err(Error::Invalid.into()); }
                        let id = graph::child_id("factorio-host", graph::WORKSPACE_KIND, &repository);
                        // Complete Access metadata establishes absence without
                        // a cold arbitrary-UUID Document read before onboarding.
                        let exists = tx.get("access.resources", &[id.clone().into()])?.is_some();
                        if exists { graph::require_owner(tx, &id, context.actor.as_deref().ok_or(Error::NotFound)?)?; }
                        context.prepared = json!({"id":id, "exists":exists});
                        Ok(())
                    })]
                },
                "factorio.agent-token" => vec![Guard::new(human)],
                "factorio.command" => vec![Guard::new(command_guard)],
                _ => vec![Guard::policy(owner)],
            },
            handler: snap_transport::operation::Handler::new(move |tx, invocation, actor, bearer, context| {
                let actor = actor.ok_or(Error::NotFound)?;
                let v = &invocation.input;
                let workspace = || text(v, "workspace");
                Ok(match name {
                    "factorio.identity" => json!({"owner":actor}),
                    "factorio.logout" => {
                        let id = rp::digest(bearer.ok_or(Error::NotFound)?);
                        if tx.get("factorio.cli", &[id.clone().into()])?.is_some() { tx.delete("factorio.cli", &[id.into()])?; }
                        else if tx.get("factorio.agents", &[id.clone().into()])?.is_some() { tx.delete("factorio.agents", &[id.into()])?; }
                        else { return Err(Error::NotFound); }
                        Value::Null
                    },
                    "factorio.repositories" => json!([{ "id":"configured", "path":config.repository, "modules":config.modules }]),
                    "factorio.onboard" => {
                        // One workspace per physical repository keeps claims and ports
                        // repository-wide. The owner may reconnect without duplicating it.
                        let id = text(&context.prepared, "id")?;
                        if context.prepared["exists"] == false { graph::onboard(tx, id, actor, config.clone())?; }
                        json!({"id":id})
                    }
                    "factorio.workspaces" => {
                        let mut roots = Vec::new();
                        let ids: Vec<String> = serde_json::from_value(context.prepared.clone()).map_err(|_| Error::Invalid)?;
                        for id in ids {
                            let snapshot = graph::document().read(tx, &id, Some(actor))?;
                            if snapshot.kind == graph::WORKSPACE_KIND { roots.push(json!({"id":id,"repository":snapshot.value["config"]["repository"]})); }
                        }
                        json!(roots)
                    }
                    "factorio.workspace" => json!(graph::load(tx, workspace()?, actor)?),
                    "factorio.command" => {
                        let command = parsed(v)?;
                        let is_human = matches!(command, Command::Approve { .. }); // guard already captured the human proof
                        graph::command(tx, workspace()?, actor, is_human, now(context)?, command)?;
                        Value::Null
                    }
                    "factorio.agent-token" => {
                        // This handler runs immediately after the guarded admission
                        // under the same gate. Read the session ID without reauthorizing.
                        let id = rp::digest(bearer.ok_or(Error::NotFound)?);
                        let token = entropy(context, "entropy")?;
                        tx.insert("factorio.agents", [("id".into(), rp::digest(&token).into()), ("session".into(), id.into())].into_iter().collect())?;
                        json!({"token":token})
                    }
                    "factorio.intake-create" => json!(graph::create_intake(tx, workspace()?, actor, text(v,"id")?, text(v,"description")?)?),
                    "factorio.intake-read" => {
                        let w = graph::load(tx, workspace()?, actor)?;
                        let item = w.intakes.get(text(v,"id")?).ok_or(Error::NotFound)?;
                        json!({"intake":item,"modules":w.config.modules,"tickets":w.tickets})
                    }
                    "factorio.intake-drafts" => json!(graph::drafts(tx, workspace()?, actor, text(v,"id")?, serde_json::from_value(v["drafts"].clone()).map_err(|_| Error::Invalid)?, now(context)?)?),
                    "factorio.intake-ready" => json!(graph::ready(tx, workspace()?, actor, text(v,"id")?, v["revision"].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or(Error::Invalid)?)?),
                    "factorio.intake-delete" => { graph::delete_intake(tx, workspace()?, actor, text(v,"id")?)?; Value::Null }
                    _ => return Err(Error::Invalid),
                })
            }),
        });
    }
    app
}
