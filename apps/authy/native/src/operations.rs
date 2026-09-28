use serde_json::json;
use snap_document_local::{Host, Request};
use snap_identity::{Crypto, Identity};
use snap_store::Error;

pub fn register(mut host: Host<snap_sqlite::Sqlite>) -> Host<snap_sqlite::Sqlite> {
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
            guard: |_, actor, _| actor.map(|_| ()).ok_or(Error::NotFound),
            handler: Box::new(move |tx, invocation, actor, bearer| {
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
