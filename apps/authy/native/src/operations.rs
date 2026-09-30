use serde_json::json;
use snap_document_local::Host;
use snap_identity::{Crypto, Identity};
use snap_store::Error;
use snap_transport::operation::Definition as Request;

pub fn register(mut host: Host<snap_sqlite::Sqlite>) -> Host<snap_sqlite::Sqlite> {
    for name in ["identity.acquire", "identity.enroll", "identity.fetch"] {
        host = host.with_http_request(Request {
            name: name.into(),
            identity_required: false,
            input: if name == "identity.fetch" {
                |v| v.is_null()
            } else {
                |v| {
                    v.as_object().is_some_and(|o| {
                        o.len() == 2 && v["email"].is_string() && v["password"].is_string()
                    })
                }
            },
            output: if name == "identity.fetch" {
                |v| v.is_null() || serde_json::from_value::<authy::Account>(v.clone()).is_ok()
            } else {
                |v| {
                    v["bearer"].is_string()
                        && serde_json::from_value::<authy::Account>(v["account"].clone()).is_ok()
                }
            },
            progress: |_| false,
            error: |_| true,
            guards: vec![],
            inputs: &[],
            tables: if name == "identity.enroll" {
                &[snap_document::server::TABLES[0]]
            } else {
                &[]
            },
            handler: snap_transport::operation::Handler::new(
                move |tx, invocation, actor, bearer, _| {
                    if name == "identity.fetch" {
                        return if actor.is_some() {
                            Ok(json!(authy::current(
                                tx,
                                &snap_crypto::Native,
                                bearer.ok_or(Error::NotFound)?,
                                crate::now()
                            )?))
                        } else {
                            Ok(serde_json::Value::Null)
                        };
                    }
                    let operation = snap_identity::operation::Operation::parse(invocation, None)
                        .map_err(|_| Error::Invalid)?
                        .ok_or(Error::Invalid)?;
                    let issued = operation.execute_with_enrollment(
                        &Identity::default(),
                        tx,
                        &mut snap_crypto::Native,
                        crate::now(),
                        authy::initialize_account,
                    )?;
                    let bearer = issued["bearer"].as_str().ok_or(Error::Invalid)?;
                    let account = authy::current(tx, &snap_crypto::Native, bearer, crate::now())?;
                    Ok(json!({"account":account,"bearer":bearer}))
                },
            ),
        });
    }
    for name in ["authy.sessions", "authy.credentials", "authy.logout"] {
        host = host.with_request(Request {
            name: name.into(),
            identity_required: true,
            input: if name == "authy.logout" {
                |v| v.as_object().is_some_and(|o| o.len() == 1 && matches!(v["scope"].as_str(), Some("current" | "others" | "all")))
            } else { |v| v.is_null() },
            output: match name {
                "authy.sessions" => |v| v.as_object().is_some_and(|o| o.len() == 1) && v["sessions"].as_array().is_some_and(|items| items.iter().all(|item| item.as_object().is_some_and(|o| o.len() == 3) && item["id"].is_string() && item["expires"].is_i64() && item["current"].is_boolean())),
                "authy.credentials" => |v| v.as_object().is_some_and(|o| o.len() == 1) && v["credentials"].as_array().is_some_and(|items| items.iter().all(|item| item.as_object().is_some_and(|o| o.len() == 3) && item["label"].is_string() && item["kind"] == "password" && item["removable"] == false)),
                _ => |v| v.is_null(),
            },
            progress: |_| false,
            error: |_| true, guards: vec![], inputs: &[],
            tables: &[],
            handler: snap_transport::operation::Handler::new(move |tx, invocation, actor, bearer, _| {
                let actor = actor.ok_or(Error::NotFound)?;
                let current = snap_crypto::Native.digest(bearer.ok_or(Error::NotFound)?);
                let identity = Identity::default();
                Ok(match invocation.operation.as_str() {
                    "authy.sessions" => json!({"sessions": identity.sessions_for(tx, &snap_crypto::Native, actor, &current, crate::now())?}),
                    "authy.credentials" => json!({"credentials": identity.credentials_for(tx, actor)?.into_iter().map(|label| json!({"label":label,"kind":"password","removable":false})).collect::<Vec<_>>()}),
                    "authy.logout" => {
                        identity.revoke_scope_for(tx, actor, &current, invocation.input["scope"].as_str().ok_or(Error::Invalid)?)?;
                        serde_json::Value::Null
                    }
                    _ => return Err(Error::Invalid),
                })
            }),
        });
    }
    host
}
