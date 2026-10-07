//! OAuth provider IO over the shared HTTP client. The blocking host supplies a
//! waiter, clock, secret resolution and crypto; this module selects no runtime,
//! network implementation, cookie codec or Store backend.
use super::acquisition::{self, Provider};
use crate::Crypto;
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use snap_http::{
    FutureValue,
    client::{Client, Outgoing, collect},
};
use snap_store::{Backend, Error};
use snap_transport::host::Controller;
use url::Url;

const MAX_BYTES: usize = 256 * 1024;
const TIMEOUT_MS: u64 = 30_000;

fn origin(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|_| Error::Invalid)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Invalid);
    }
    Ok(url)
}
fn issuer(value: &str) -> Result<Url, Error> {
    let url = origin(value)?;
    // Preserve the current relying-party policy: issuers are canonical origins.
    if url.path() != "/" || url.query().is_some() || url.origin().ascii_serialization() != value {
        return Err(Error::Invalid);
    }
    Ok(url)
}
fn endpoint(provider: &Provider, value: &str) -> Result<String, Error> {
    let url = origin(value)?;
    if url.origin() != issuer(&provider.issuer)?.origin() {
        return Err(Error::Invalid);
    }
    Ok(url.into())
}
pub(super) fn validate_providers(providers: &[Provider]) -> Result<(), Error> {
    for (index, p) in providers.iter().enumerate() {
        if p.name.is_empty()
            || !p
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || providers[..index].iter().any(|old| old.name == p.name)
            || p.client.is_empty()
            || p.continuations.is_empty()
        {
            return Err(Error::Invalid);
        }
        issuer(&p.issuer)?;
        endpoint(p, &p.authorization_endpoint)?;
        for continuation in p.continuations.values() {
            origin(continuation)?;
        }
    }
    Ok(())
}

fn outgoing(method: &'static str, url: String) -> Outgoing {
    Outgoing {
        method,
        url,
        headers: vec![("accept".into(), "application/json".into())],
        body: Vec::new(),
        max_bytes: MAX_BYTES,
        timeout_ms: TIMEOUT_MS,
    }
}
fn json<C: Client>(client: C, request: Outgoing) -> FutureValue<Result<Value, Error>> {
    Box::pin(async move {
        let mut response = client.send(request).await.map_err(|_| Error::Unavailable)?;
        if response.status != 200 {
            return Err(Error::NotFound);
        }
        let bytes = collect(&mut response.body, MAX_BYTES)
            .await
            .map_err(|_| Error::Unavailable)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
    })
}
fn basic(client: &str, secret: &str) -> String {
    let encode = |value: &str| {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("v", value)
            .finish()[2..]
            .to_string()
    };
    format!(
        "Basic {}",
        STANDARD.encode(format!("{}:{}", encode(client), encode(secret)))
    )
}

/// Assemble the production verification controller. `wait` drives shared HTTP
/// futures on the host's blocking execution thread; it must not reenter dispatch.
/// The FIFO remains held, but no transaction spans IO. The supplied Client must
/// enforce request deadlines and never follow redirects or automatically retry.
/// Secret resolution uses configured provider names, never callback input URLs.
/// Run `oauth::recover` before controller recovery and opening listeners.
pub fn controller<B: Backend, C: Client, K: Crypto>(
    providers: Vec<Provider>,
    client: C,
    crypto: impl Fn() -> K + Send + 'static,
    mut secret: impl FnMut(&str) -> Result<String, Error> + Send + 'static,
    clock: impl Fn() -> i64 + Send + 'static,
    mut wait: impl FnMut(FutureValue<Result<Value, Error>>) -> Result<Value, Error> + Send + 'static,
) -> Result<Controller<B>, Error> {
    validate_providers(&providers)?;
    Ok(Controller::new(
        "identity.oauth-verification",
        super::TABLES[0],
        acquisition::pending,
        move |ctx, resource| {
            let [snap_store::Value::Text(key)] = resource.key.as_slice() else {
                return Err(Error::Invalid);
            };
            // A failed claim never calls HTTP. A successful claim stays fenced
            // through every failure, including an uncertain token exchange.
            let exchange = ctx.transact("oauth.exchange-claim", |tx| {
                acquisition::claim(tx, key, clock())
            })?;
            let result = (|| {
                let provider = providers
                    .iter()
                    .find(|p| p.name == exchange.provider())
                    .ok_or(Error::Invalid)?;
                let credential = secret(&provider.name)?;
                if credential.len() < 32 {
                    return Err(Error::Invalid);
                }
                let metadata = wait(json(
                    client.clone(),
                    outgoing(
                        "GET",
                        format!("{}/.well-known/openid-configuration", provider.issuer),
                    ),
                ))?;
                if metadata["issuer"].as_str() != Some(&provider.issuer) {
                    return Err(Error::Invalid);
                }
                let at =
                    |name: &str| endpoint(provider, metadata[name].as_str().ok_or(Error::Invalid)?);
                if at("authorization_endpoint")?
                    != endpoint(provider, &provider.authorization_endpoint)?
                {
                    return Err(Error::Invalid);
                }
                // Pin all destinations before transmitting code or credentials.
                let token_endpoint = at("token_endpoint")?;
                let jwks_uri = at("jwks_uri")?;
                let userinfo_endpoint = at("userinfo_endpoint")?;
                let attempt = exchange.attempt();
                let mut request = outgoing("POST", token_endpoint);
                request.headers.extend([
                    ("authorization".into(), basic(&provider.client, &credential)),
                    (
                        "content-type".into(),
                        "application/x-www-form-urlencoded".into(),
                    ),
                ]);
                request.body = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs([
                        ("grant_type", "authorization_code"),
                        ("code", exchange.code()),
                        ("redirect_uri", attempt.redirect.as_str()),
                        ("code_verifier", attempt.verifier.as_str()),
                    ])
                    .finish()
                    .into_bytes();
                let response = wait(json(client.clone(), request))?;
                let jwks = wait(json(client.clone(), outgoing("GET", jwks_uri)))?;
                // Verify before using the upstream bearer, even at a pinned URL.
                let verified =
                    acquisition::verify(&crypto(), &exchange, provider, &response, &jwks, clock())?;
                let mut request = outgoing("GET", userinfo_endpoint);
                request.headers.push((
                    "authorization".into(),
                    format!("Bearer {}", verified.tokens.access),
                ));
                let profile = wait(json(client.clone(), request))?;
                ctx.transact("oauth.exchange-verified", |tx| {
                    acquisition::record_verified(tx, &exchange, verified, profile, clock())
                })
            })();
            if result.is_err() {
                // Never log provider bodies or request credentials. A confirmed
                // failure is private state; the operation emits its typed error.
                ctx.transact("oauth.exchange-failed", |tx| {
                    acquisition::failed(tx, &exchange)
                })?;
            }
            Ok(())
        },
    ))
}

/// Renewal uses the same pinned discovery and proof verification as acquisition,
/// but may update tokens only on an unexpired, already-authorized local grant.
pub fn renewal_controller<B: Backend, C: Client, K: Crypto>(
    providers: Vec<Provider>,
    client: C,
    crypto: impl Fn() -> K + Send + 'static,
    mut secret: impl FnMut(&str) -> Result<String, Error> + Send + 'static,
    clock: impl Fn() -> i64 + Send + 'static,
    mut wait: impl FnMut(FutureValue<Result<Value, Error>>) -> Result<Value, Error> + Send + 'static,
) -> Result<Controller<B>, Error> {
    validate_providers(&providers)?;
    // A grant records the issuer, not a client-selected provider name. Ambiguous
    // issuer mappings cannot safely select a refresh client credential.
    if providers
        .iter()
        .enumerate()
        .any(|(i, p)| providers[..i].iter().any(|old| old.issuer == p.issuer))
    {
        return Err(Error::Invalid);
    }
    Ok(Controller::new(
        "identity.oauth-renewal",
        super::ATTEMPTS,
        super::renewal::pending,
        move |ctx, resource| {
            let [snap_store::Value::Text(key)] = resource.key.as_slice() else {
                return Err(Error::Invalid);
            };
            let previous = ctx.transact("oauth.renewal-claim", |tx| {
                super::renewal::claim(tx, key, clock())
            })?;
            let result = (|| {
                let p = providers
                    .iter()
                    .find(|p| p.issuer == previous.issuer)
                    .ok_or(Error::Invalid)?;
                let credential = secret(&p.name)?;
                if credential.len() < 32 {
                    return Err(Error::Invalid);
                }
                let metadata = wait(json(
                    client.clone(),
                    outgoing(
                        "GET",
                        format!("{}/.well-known/openid-configuration", p.issuer),
                    ),
                ))?;
                if metadata["issuer"].as_str() != Some(&p.issuer) {
                    return Err(Error::Invalid);
                }
                let at = |name: &str| endpoint(p, metadata[name].as_str().ok_or(Error::Invalid)?);
                if at("authorization_endpoint")? != endpoint(p, &p.authorization_endpoint)? {
                    return Err(Error::Invalid);
                }
                let token_endpoint = at("token_endpoint")?;
                let keys_endpoint = at("jwks_uri")?;
                let mut request = outgoing("POST", token_endpoint);
                request.headers.extend([
                    ("authorization".into(), basic(&p.client, &credential)),
                    (
                        "content-type".into(),
                        "application/x-www-form-urlencoded".into(),
                    ),
                ]);
                request.body = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs([
                        ("grant_type", "refresh_token"),
                        ("refresh_token", previous.tokens.refresh.as_str()),
                    ])
                    .finish()
                    .into_bytes();
                let response = wait(json(client.clone(), request))?;
                let jwks = wait(json(client.clone(), outgoing("GET", keys_endpoint)))?;
                let claims = crypto()
                    .verify_token(response["id_token"].as_str().ok_or(Error::Invalid)?, &jwks)?;
                let checked_at = clock();
                let tokens = super::validate_tokens(
                    &response,
                    &claims,
                    super::Validation {
                        issuer: &p.issuer,
                        client: &p.client,
                        nonce: None,
                        previous: Some(&previous),
                        now: checked_at,
                    },
                )?;
                ctx.transact("oauth.renewal-finish", |tx| {
                    let now = clock();
                    if now < checked_at || claims["exp"].as_i64().is_none_or(|exp| exp <= now) {
                        return Err(Error::NotFound);
                    }
                    super::finish_refresh(tx, &previous, tokens, now)?;
                    super::renewal::clear(tx, key)
                })
            })();
            if result.is_err() {
                ctx.transact("oauth.renewal-failed", |tx| {
                    super::revoke_id(tx, &previous.id)?;
                    super::renewal::clear(tx, key)
                })?;
            }
            Ok(())
        },
    ))
}
