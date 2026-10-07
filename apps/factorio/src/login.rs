//! Login credentials and delegation. Platform bootstrap owns OAuth IO and entropy.
use crate::operations::{entropy, now, request, text};
use alloc::{format, string::String, vec, vec::Vec};
use serde_json::{Value, json};
use snap_identity::oauth as rp;
use snap_store::{Error, Transaction};
use snap_transport::{
    Invocation,
    operation::{Context, Definition, Failure, Guard, Handler},
};

/// Residency of the local login/delegation data interface.
pub(crate) fn data() -> snap_store::Data {
    snap_store::Data::new(&["factorio.cli_login", "factorio.cli", "factorio.agents"])
        .and(rp::data())
}

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
) -> Result<(rp::Grant, bool), Error> {
    let (id, human) = session_id(tx, bearer, now)?;
    Ok((rp::lease(tx, &id, now)?, human))
}

/// Recovery retention cannot authorize operations or extend either login expiry.
pub fn retained(tx: &mut Transaction<'_>, bearer: &str, now: i64) -> Result<String, Error> {
    let (id, _) = session_id(tx, bearer, now)?;
    rp::retained(tx, &id, now).map(|s| s.owner)
}

pub(crate) fn human(
    tx: &mut Transaction<'_>,
    _: &Invocation,
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

fn finish_guard(
    tx: &mut Transaction<'_>,
    call: &Invocation,
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

pub(crate) fn declarations(origin: String) -> Vec<Definition> {
    vec![
        Definition {
            name: "factorio.login-start".into(),
            http: None,
            identity_required: false,
            input: Value::is_object,
            output: |v| v.is_object() || v.is_null(),
            progress: |_| false,
            error: |_| true,
            guards: vec![],
            data: data(),
            inputs: &["clock", "entropy", "proof"],
            handler: Handler::new(move |tx, _, _, _, context| {
                let now = now(context)?;
                let rows = tx.find("factorio.cli_login", "primary", &[])?;
                let mut active = 0;
                for row in rows {
                    if let (
                        Some(snap_store::Value::Text(id)),
                        Some(snap_store::Value::Integer(expires)),
                    ) = (row.get("id"), row.get("expires"))
                    {
                        if *expires <= now {
                            tx.delete("factorio.cli_login", &[id.clone().into()])?;
                        } else {
                            active += 1;
                        }
                    }
                }
                if active >= 128 {
                    return Err(Error::Unavailable);
                }
                let code = entropy(context, "entropy")?;
                let proof = entropy(context, "proof")?;
                let expires = now + 300;
                tx.insert(
                    "factorio.cli_login",
                    [
                        ("id".into(), code.clone().into()),
                        ("proof".into(), rp::digest(&proof).into()),
                        ("session".into(), "".into()),
                        ("expires".into(), expires.into()),
                    ]
                    .into_iter()
                    .collect(),
                )?;
                Ok(
                    json!({"code":code,"proof":proof,"expires":expires,"url":format!("{origin}/auth/cli/{code}")}),
                )
            }),
        },
        Definition {
            name: "factorio.login-finish".into(),
            http: None,
            identity_required: false,
            input: Value::is_object,
            output: |v| v.is_object() || v.is_null(),
            progress: |_| false,
            error: |_| true,
            guards: vec![Guard::new(finish_guard)],
            data: data(),
            inputs: &["clock", "entropy"],
            handler: Handler::new(|tx, _, _, _, context| {
                let now = now(context)?;
                let prepared = &context.prepared;
                if prepared["pending"] == true {
                    return Ok(Value::Null);
                }
                let code = text(prepared, "code")?;
                let session = text(prepared, "session")?;
                let session_expires = prepared["expires"].as_i64().ok_or(Error::Invalid)?;
                let token = entropy(context, "entropy")?;
                let expires = (now + CLI_LOGIN_SECONDS).min(session_expires);
                tx.insert(
                    "factorio.cli",
                    [
                        ("id".into(), rp::digest(&token).into()),
                        ("session".into(), session.into()),
                        ("expires".into(), expires.into()),
                    ]
                    .into_iter()
                    .collect(),
                )?;
                tx.delete("factorio.cli_login", &[code.into()])?;
                Ok(json!({"bearer":token,"expires":expires,"owner":prepared["owner"]}))
            }),
        },
        Definition {
            name: "factorio.login".into(),
            http: None,
            identity_required: true,
            input: Value::is_null,
            output: |v| v["bearer"].is_string() && v["expires"].is_i64() && v["owner"].is_string(),
            progress: |_| false,
            error: |_| true,
            data: data(),
            inputs: &["clock", "entropy"],
            guards: vec![Guard::new(|tx, _, context| {
                // Only an ordinary agent token may delegate CLI authority. A CLI
                // credential cannot delegate or extend its backing OAuth grant.
                let bearer = context.bearer.as_deref().ok_or(Error::NotFound)?;
                tx.get("factorio.agents", &[rp::digest(bearer).into()])?
                    .ok_or(Error::NotFound)?;
                let (session, _) = session(tx, bearer, now(context)?)?;
                context.prepared =
                    json!({"id":session.id,"expires":session.expires,"owner":session.owner});
                Ok(())
            })],
            handler: Handler::new(|tx, _, _, _, context| {
                let s = &context.prepared;
                let id = text(s, "id")?;
                let owner = text(s, "owner")?;
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
        },
    ]
}

pub(crate) fn agent_token() -> Definition {
    let mut definition = request(
        "factorio.agent-token",
        |v| v.as_object().is_some_and(|o| o.len() == 1) && v["token"].is_string(),
        data(),
        vec![Guard::new(human)],
        Handler::new(|tx, _, actor, bearer, context| {
            actor.ok_or(Error::NotFound)?;
            // Admission captured human proof under this gate; do not reauthorize.
            let id = rp::digest(bearer.ok_or(Error::NotFound)?);
            let token = entropy(context, "entropy")?;
            tx.insert(
                "factorio.agents",
                [
                    ("id".into(), rp::digest(&token).into()),
                    ("session".into(), id.into()),
                ]
                .into_iter()
                .collect(),
            )?;
            Ok(json!({"token":token}))
        }),
    );
    definition.inputs = &["clock", "entropy"];
    definition
}

pub(crate) fn logout() -> Definition {
    request(
        "factorio.logout",
        Value::is_null,
        data(),
        vec![],
        Handler::new(|tx, _, actor, bearer, _| {
            actor.ok_or(Error::NotFound)?;
            let id = rp::digest(bearer.ok_or(Error::NotFound)?);
            if tx.get("factorio.cli", &[id.clone().into()])?.is_some() {
                tx.delete("factorio.cli", &[id.into()])?;
            } else if tx.get("factorio.agents", &[id.clone().into()])?.is_some() {
                tx.delete("factorio.agents", &[id.into()])?;
            } else {
                return Err(Error::NotFound);
            }
            Ok(Value::Null)
        }),
    )
}
