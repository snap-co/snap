//! Portable account requests. Hosts supply crypto and the declared Unix clock.
use alloc::{vec, vec::Vec};
use serde_json::{Value, json};
use snap_identity::{Crypto, Identity};
use snap_store::Error;
use snap_transport::operation::{Context, Definition, Handler, Validator};

pub struct Application {
    pub requests: Vec<Definition>,
    pub preconnection: Vec<Definition>,
}

fn now(context: &Context) -> Result<i64, Error> {
    context
        .inputs
        .get("clock")
        .and_then(Value::as_i64)
        .ok_or(Error::Unavailable)
}

fn definition(
    name: &str,
    identity_required: bool,
    input: Validator,
    output: Validator,
    tables: &'static [&'static str],
    inputs: &'static [&'static str],
    handler: Handler,
) -> Definition {
    Definition {
        name: name.into(),
        identity_required,
        input,
        output,
        tables,
        handler,
        progress: |_| false,
        error: |_| true,
        guards: vec![],
        inputs,
    }
}

/// The crypto factory is selected once at assembly. It supplies a request-local
/// implementation of Identity's portable interface, never a native dependency.
/// Enrollment commits credentials, session, profile and metadata together.
pub fn declarations<C: Crypto>(crypto: impl Fn() -> C + Clone + Send + 'static) -> Application {
    let acquire = crypto.clone();
    let enroll = crypto.clone();
    let fetch = crypto.clone();
    let sessions = crypto.clone();
    let credential_input: Validator = |v| {
        v.as_object()
            .is_some_and(|o| o.len() == 2 && v["email"].is_string() && v["password"].is_string())
    };
    let issued_output: Validator = |v| {
        v["bearer"].is_string()
            && serde_json::from_value::<crate::Account>(v["account"].clone()).is_ok()
    };
    Application {
        preconnection: vec![
            definition(
                "identity.acquire",
                false,
                credential_input,
                issued_output,
                &[],
                &["clock"],
                Handler::new(move |tx, call, _, _, context| {
                    let mut crypto = acquire();
                    let v = &call.input;
                    let issued = Identity::default().login(
                        tx,
                        &mut crypto,
                        v["email"].as_str().ok_or(Error::Invalid)?,
                        v["password"].as_str().ok_or(Error::Invalid)?,
                        now(context)?,
                    )?;
                    let account = crate::current(tx, &crypto, &issued.bearer, now(context)?)?;
                    Ok(json!({"account":account,"bearer":issued.bearer}))
                }),
            ),
            definition(
                "identity.enroll",
                false,
                credential_input,
                issued_output,
                &[snap_document::server::TABLES[0]],
                &["clock"],
                Handler::new(move |tx, call, _, _, context| {
                    let mut crypto = enroll();
                    let v = &call.input;
                    let issued = crate::enroll(
                        tx,
                        &mut crypto,
                        v["email"].as_str().ok_or(Error::Invalid)?,
                        v["password"].as_str().ok_or(Error::Invalid)?,
                        now(context)?,
                    )?;
                    let account = crate::current(tx, &crypto, &issued.bearer, now(context)?)?;
                    Ok(json!({"account":account,"bearer":issued.bearer}))
                }),
            ),
            definition(
                "identity.fetch",
                false,
                Value::is_null,
                |v| v.is_null() || serde_json::from_value::<crate::Account>(v.clone()).is_ok(),
                &[],
                &["clock"],
                Handler::new(move |tx, _, actor, bearer, context| {
                    if actor.is_some() {
                        Ok(json!(crate::current(
                            tx,
                            &fetch(),
                            bearer.ok_or(Error::NotFound)?,
                            now(context)?
                        )?))
                    } else {
                        Ok(Value::Null)
                    }
                }),
            ),
        ],
        requests: vec![
            definition(
                "authy.sessions",
                true,
                Value::is_null,
                |v| {
                    v.as_object().is_some_and(|o| o.len() == 1)
                        && v["sessions"].as_array().is_some_and(|items| {
                            items.iter().all(|item| {
                                item.as_object().is_some_and(|o| o.len() == 3)
                                    && item["id"].is_string()
                                    && item["expires"].is_i64()
                                    && item["current"].is_boolean()
                            })
                        })
                },
                &[],
                &["clock"],
                Handler::new(move |tx, _, actor, bearer, context| {
                    let actor = actor.ok_or(Error::NotFound)?;
                    let crypto = sessions();
                    let current = crypto.digest(bearer.ok_or(Error::NotFound)?);
                    Ok(
                        json!({"sessions":Identity::default().sessions_for(tx, &crypto, actor, &current, now(context)?)?}),
                    )
                }),
            ),
            definition(
                "authy.credentials",
                true,
                Value::is_null,
                |v| {
                    v.as_object().is_some_and(|o| o.len() == 1)
                        && v["credentials"].as_array().is_some_and(|items| {
                            items.iter().all(|item| {
                                item.as_object().is_some_and(|o| o.len() == 3)
                                    && item["label"].is_string()
                                    && item["kind"] == "password"
                                    && item["removable"] == false
                            })
                        })
                },
                &[],
                &[],
                Handler::new(|tx, _, actor, bearer, _| {
                    let actor = actor.ok_or(Error::NotFound)?;
                    bearer.ok_or(Error::NotFound)?;
                    Ok(
                        json!({"credentials":Identity::default().credentials_for(tx, actor)?.into_iter().map(|label| json!({"label":label,"kind":"password","removable":false})).collect::<Vec<_>>()}),
                    )
                }),
            ),
            definition(
                "authy.logout",
                true,
                |v| {
                    v.as_object().is_some_and(|o| {
                        o.len() == 1
                            && matches!(v["scope"].as_str(), Some("current" | "others" | "all"))
                    })
                },
                Value::is_null,
                &[],
                &[],
                Handler::new(move |tx, call, actor, bearer, _| {
                    let actor = actor.ok_or(Error::NotFound)?;
                    let current = crypto().digest(bearer.ok_or(Error::NotFound)?);
                    Identity::default().revoke_scope_for(
                        tx,
                        actor,
                        &current,
                        call.input["scope"].as_str().ok_or(Error::Invalid)?,
                    )?;
                    Ok(Value::Null)
                }),
            ),
        ],
    }
}
