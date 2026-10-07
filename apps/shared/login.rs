//! App-owned browser projection of Identity's declared operations.
use axum::{http::HeaderMap, response::Response};
use snap_identity::oauth::{acquisition as flow, release};
use snap_transport::{
    Error, Value,
    native::web::{Cookies, HttpOperation, WriteCookie, cookie, redirect},
};
use std::sync::Arc;

fn continuation(
    headers: &HeaderMap,
    origin: &str,
    dev_origins: &[String],
) -> Result<String, Error> {
    if dev_origins.is_empty() {
        return Ok(origin.into());
    }
    match headers.get("x-snap-dev-origin") {
        None => Ok(origin.into()),
        Some(value) => dev_origins
            .iter()
            .find(|o| value.to_str().ok() == Some(o.as_str()))
            .cloned()
            .ok_or(Error::InvalidInput),
    }
}
fn authorization(output: &Value, cookies: &Cookies) -> Result<Response, Error> {
    let a: flow::Authorization =
        serde_json::from_value(output.clone()).map_err(|_| Error::InvalidOutput)?;
    let mut target = url::Url::parse(&a.endpoint).map_err(|_| Error::InvalidOutput)?;
    target.query_pairs_mut().extend_pairs([
        ("client_id", a.client.as_str()),
        ("redirect_uri", &a.redirect),
        ("response_type", "code"),
        ("scope", "openid profile email"),
        ("state", &a.state),
        ("nonce", &a.nonce),
        ("code_challenge", &a.code_challenge),
        ("code_challenge_method", "S256"),
    ]);
    let mut response = redirect(target.as_str())?;
    cookie(&mut response, cookies.encode(Some(&a.binding), true))?;
    Ok(response)
}
pub fn operations(
    cookies: Cookies,
    provider: &flow::Provider,
    origin: &str,
    dev_origins: &[String],
) -> Vec<HttpOperation> {
    let write: WriteCookie = {
        let cookies = cookies.clone();
        Arc::new(move |bearer| cookies.encode(bearer, false))
    };
    let name = provider.name.clone();
    let release_origin = origin.to_owned();
    let origin = origin.to_owned();
    let dev = dev_origins.to_vec();
    let begin_input = move |headers: &HeaderMap, input: Value| {
        if !input.is_null() && input != serde_json::json!({}) {
            return Err(Error::InvalidInput);
        }
        serde_json::to_value(flow::BeginInput {
            provider: name.clone(),
            continuation: continuation(headers, &origin, &dev)?,
        })
        .map_err(|_| Error::InvalidInput)
    };
    let decoder = begin_input.clone();
    let encode = cookies.clone();
    let begin = HttpOperation::for_operation::<flow::Begin>(write.clone())
        .parameters("/auth/login", move |headers, params| {
            if !params.is_empty() {
                return Err(Error::InvalidInput);
            }
            decoder(headers, Value::Null)
        })
        .response(move |output| authorization(output, &encode));
    let encode = cookies.clone();
    let link = HttpOperation::for_operation::<flow::Link>(write.clone())
        .at("/auth/link")
        .input(begin_input)
        .response(move |output| authorization(output, &encode));
    // Link and acquisition share one callback. Their private attempt records
    // capture the intent and linking target; callback fields cannot select either.
    let read = cookies.clone();
    let encode = cookies.clone();
    let name = provider.name.clone();
    let callback = HttpOperation::for_operation::<flow::Callback>(write.clone())
        .parameters("/auth/callback", move |headers, mut params| {
            let binding = read.read(headers, true).ok_or(Error::InvalidBearer)?;
            let state = params.remove("state").ok_or(Error::InvalidInput)?;
            let issuer = params.remove("iss").ok_or(Error::InvalidInput)?;
            let response = match (params.remove("code"), params.remove("error")) {
                (Some(code), None) => flow::ProviderResponse::Code { code },
                (None, Some(error)) => flow::ProviderResponse::Rejected { error },
                _ => return Err(Error::InvalidInput),
            };
            params.remove("error_description");
            params.remove("error_uri");
            if !params.is_empty() {
                return Err(Error::InvalidInput);
            }
            serde_json::to_value(flow::CallbackInput {
                provider: name.clone(),
                state,
                binding,
                issuer,
                response,
            })
            .map_err(|_| Error::InvalidInput)
        })
        .form_post(provider.issuer.clone())
        .response(move |_| {
            let mut response = redirect("/")?;
            cookie(&mut response, encode.encode(None, true))?;
            Ok(response)
        });
    let name = provider.name.clone();
    let origin = release_origin;
    let dev = dev_origins.to_vec();
    let encode = cookies.clone();
    let logout = HttpOperation::for_operation::<release::Release>(write.clone())
        .at("/auth/logout")
        .input(move |headers, input| {
            if !input.is_null() && input != serde_json::json!({}) {
                return Err(Error::InvalidInput);
            }
            serde_json::to_value(release::Input {
                provider: name.clone(),
                continuation: continuation(headers, &origin, &dev)?,
                csrf: headers
                    .get("x-snap-csrf")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(Error::InvalidInput)?
                    .into(),
            })
            .map_err(|_| Error::InvalidInput)
        })
        .response(move |output| {
            let c: release::Continuation =
                serde_json::from_value(output.clone()).map_err(|_| Error::InvalidOutput)?;
            let mut target = url::Url::parse(&c.endpoint).map_err(|_| Error::InvalidOutput)?;
            target.query_pairs_mut().extend_pairs([
                ("client_id", c.client.as_str()),
                ("post_logout_redirect_uri", &c.redirect),
                ("state", &c.state),
            ]);
            let mut response = super::no_store(serde_json::json!({"redirect":target.as_str()}));
            cookie(&mut response, encode.encode(Some(&c.binding), true))?;
            Ok(response)
        });
    let read = cookies.clone();
    let returned = HttpOperation::for_operation::<release::Returned>(write)
        .parameters("/auth/logged-out", move |headers, mut params| {
            let state = params.remove("state").ok_or(Error::InvalidInput)?;
            if !params.is_empty() {
                return Err(Error::InvalidInput);
            }
            serde_json::to_value(release::ReturnInput {
                state,
                binding: read.read(headers, true).ok_or(Error::InvalidBearer)?,
            })
            .map_err(|_| Error::InvalidInput)
        })
        .response(move |_| {
            let mut response = redirect("/")?;
            cookie(&mut response, cookies.encode(None, true))?;
            Ok(response)
        });
    vec![begin, link, callback, logout, returned]
}
