//! Explicit application composition. Each declaration owns its handler and policy.
use crate::{intake, login, workspaces};
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use serde_json::Value;
use snap_store::Error;
use snap_transport::operation::{Context, Definition, Guard, Handler, Validator};

pub fn declarations(config: crate::Config, origin: String) -> crate::Application {
    let mut requests = workspaces::operations::declarations(config);
    requests.extend(intake::operations::declarations());
    crate::Application {
        requests,
        preconnection: login::declarations(origin),
    }
}

/// Authenticated requests share the object-size bound and platform clock input.
/// Feature declarations explicitly supply policy, residency and output validation.
pub(crate) fn request(
    name: &str,
    output: Validator,
    data: snap_store::Data,
    guards: Vec<Guard>,
    handler: Handler,
) -> Definition {
    Definition {
        name: name.into(),
        identity_required: true,
        input: |v| v.is_object() && v.to_string().len() <= 60000,
        output,
        progress: |_| false,
        error: |_| true,
        data,
        inputs: &["clock"],
        guards,
        handler,
    }
}

pub(crate) fn now(context: &Context) -> Result<i64, Error> {
    context
        .inputs
        .get("clock")
        .and_then(Value::as_i64)
        .ok_or(Error::Unavailable)
}

pub(crate) fn entropy(context: &Context, key: &str) -> Result<String, Error> {
    context
        .inputs
        .get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or(Error::Unavailable)
}

pub(crate) fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, Error> {
    v[key].as_str().ok_or(Error::Invalid)
}
